//! Generates independent traces from a supplied firmware checkout and optional full CMSIS SDK.

mod ffi;
mod trace;

#[cfg(test)]
mod tests;

use regex::Regex;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

const CMSIS_SOURCE_NAMES: [&str; 7] = [
    "ControllerFunctions/arm_sin_cos_f32.c",
    "FastMathFunctions/arm_atan2_f32.c",
    "BasicMathFunctions/arm_dot_prod_f32.c",
    "BasicMathFunctions/arm_scale_f32.c",
    "BasicMathFunctions/arm_clip_f32.c",
    "SupportFunctions/arm_fill_f32.c",
    "CommonTables/arm_common_tables.c",
];

const FIRMWARE_SOURCES: [&str; 3] = [
    "src/sensor/fusion/vqf/vqf.c",
    "src/sensor/calibration/tcal_mls_lut.c",
    "vqf-c/src/vqf.c",
];

const PREAMBLE: &str = r#"
#include "vqf.h"
#include <math.h>
#include <stdint.h>
#include <stdbool.h>
#include <string.h>
static vqf_params_t params;
static vqf_state_t state;
static vqf_coeffs_t coeffs;
static bool rest_observation_pending;
static float last_a[3];
#define ARG_UNUSED(x) ((void)(x))
#define IS_ENABLED(x) 0
#define DEG_TO_RAD 0.01745329251994329577f
#define CONST_EARTH_GRAVITY 9.80665f
#define ALWAYS_INLINE inline
static void vqf_track_rest_diag(void) {}
#define MLS_MAX_POINTS 10
#define MLS_CACHE_SLOTS 5
#define MLS_CACHE_TEMP_THRESHOLD 0.5f
#define MLS_BANDWIDTH 2.5f
#define MLS_MIN_WEIGHT 0.03f
#define TCAL_BUFFER_SIZE 4
#define LOG_ERR(...) ((void)0)
#define LOG_DBG(...) ((void)0)
static uint32_t model_generation;
typedef struct {float temp,bias[3],slope[3];bool valid;} MlsCacheSlot;
static struct {MlsCacheSlot slots[5];uint32_t generation;} mls_cache;
static struct {struct {int count;} tempCalState;struct {float temp,bias[3];} tempCalPoints[4];} retained_value;
#define retained (&retained_value)
static bool v_finite(const float *v, int count) {for(int i=0;i<count;i++) if(!isfinite(v[i]))return false;return true;}
"#;

const WRAPPER: &str = r#"
void oracle_init(void) {
 set_params();initVqf(&params,&state,&coeffs,.01f,.01f,.02f);
 memset(&mls_cache,0,sizeof(mls_cache));retained->tempCalState.count=4;
 const float temps[4]={-20,-10,10,20};
 for(int i=0;i<4;i++){retained->tempCalPoints[i].temp=temps[i];for(int j=0;j<3;j++)retained->tempCalPoints[i].bias[j]=temps[i]*(j+1)*.1f;}
}
void oracle_step(float *g,float *a,float *m,float mag_dt,float temp,int thermal_on,float *out) {
 float corrected[3];memcpy(corrected,g,sizeof(corrected));
 if(thermal_on){float offset[3];sensor_tcal_mls_lookup_locked(temp,offset);for(int j=0;j<3;j++)corrected[j]=(g[j]-offset[j])*(1.f+.1f*j);}
 vqf_update_gyro(corrected,.01f);vqf_update_accel(a,.01f);
 if(m[0]!=0||m[1]!=0||m[2]!=0)vqf_update_mag(m,mag_dt);
 getQuat6D(&state,out);getQuat9D(&state,out+4);
 memcpy(out+8,state.bias,3*sizeof(float));out[11]=state.delta;
 out[12]=state.restDetected;out[13]=state.magDistDetected;
 out[14]=state.magNormDip[0];out[15]=state.magRefNorm;out[16]=state.magNormDip[1];out[17]=state.magRefDip;
 out[18]=state.biasP[0];out[19]=state.biasP[4];out[20]=state.biasP[8];out[21]=state.kMagInit;
}
"#;

const HELP: &str = "Usage: slimenrf-tools reference-oracle --firmware PATH --output PATH [--cmsis-dsp PATH] [--compare PATH]\n\n\
Generate independent traces from the actual firmware wrapper, VQF engine and thermal MLS.\n\
  --firmware PATH    firmware source root including vqf-c\n\
  --output PATH      output CSV and adjacent provenance JSON\n\
  --cmsis-dsp PATH   full SDK modules/lib/cmsis-dsp root (omit for portable baseline)\n\
  --compare PATH     compare with a portable baseline CSV\n\
  -h, --help         show this help\n\n\
Requires host GCC. Controlled scalar CMSIS is not exact target compiler/FPU equivalence.";

#[derive(Debug)]
struct Options {
    firmware: PathBuf,
    output: PathBuf,
    cmsis_dsp: Option<PathBuf>,
    compare: Option<PathBuf>,
}

#[derive(Debug)]
enum Arguments {
    Help,
    Run(Options),
}

fn parse_args(args: &[OsString]) -> Result<Arguments, String> {
    let mut firmware = None;
    let mut output = None;
    let mut cmsis_dsp = None;
    let mut compare = None;
    let mut index = 0;
    while index < args.len() {
        let argument = args[index]
            .to_str()
            .ok_or_else(|| format!("invalid option: {:?}", args[index]))?;
        if argument == "-h" || argument == "--help" {
            return Ok(Arguments::Help);
        }
        let (option, inline) = argument
            .split_once('=')
            .map_or((argument, None), |(option, value)| (option, Some(value)));
        let target = match option {
            "--firmware" => &mut firmware,
            "--output" => &mut output,
            "--cmsis-dsp" => &mut cmsis_dsp,
            "--compare" => &mut compare,
            _ => return Err(format!("unrecognized argument: {argument}")),
        };
        let value = if let Some(value) = inline {
            OsString::from(value)
        } else {
            index += 1;
            args.get(index)
                .ok_or_else(|| format!("{option} requires a path"))?
                .clone()
        };
        *target = Some(PathBuf::from(value));
        index += 1;
    }
    Ok(Arguments::Run(Options {
        firmware: firmware.ok_or("--firmware is required")?,
        output: output.ok_or("--output is required")?,
        cmsis_dsp,
        compare,
    }))
}

/// Run the independent firmware trace generator.
///
/// # Errors
/// Returns an error for invalid arguments, missing inputs, compiler or I/O failures,
/// or incompatible comparison traces.
pub fn run(args: &[OsString]) -> Result<(), String> {
    let options = match parse_args(args)? {
        Arguments::Help => {
            println!("{HELP}");
            return Ok(());
        }
        Arguments::Run(options) => options,
    };
    generate(&options)
}

fn source_text(path: &Path) -> Result<String, String> {
    let source =
        fs::read_to_string(path).map_err(|error| format!("reading {}: {error}", path.display()))?;
    // Python's text-mode input normalized both CRLF and standalone CR before extraction.
    Ok(source.replace("\r\n", "\n").replace('\r', "\n"))
}

fn extract<'a>(source: &'a str, name: &str) -> Result<&'a str, String> {
    let expression = format!(
        r"(?m)^(?:static )?(?:ALWAYS_INLINE )?(?:void|bool|float|int) {}\([^;{{]*\)\s*\{{",
        regex::escape(name)
    );
    let pattern = Regex::new(&expression).map_err(|error| error.to_string())?;
    let matched = pattern
        .find(source)
        .ok_or_else(|| format!("Missing production function: {name}"))?;
    let mut depth = 1usize;
    // As in the original oracle, production function bodies contain no braces in string literals.
    for (offset, byte) in source.as_bytes()[matched.end()..].iter().enumerate() {
        match byte {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Ok(&source[matched.start()..matched.end() + offset + 1]);
                }
            }
            _ => {}
        }
    }
    Err(format!("Unterminated production function: {name}"))
}

fn wrapper_source(firmware: &Path) -> Result<String, String> {
    let wrapper = source_text(&firmware.join(FIRMWARE_SOURCES[0]))?;
    let thermal = source_text(&firmware.join(FIRMWARE_SOURCES[1]))?;
    let mut source = String::from(PREAMBLE);
    for (index, name) in [
        "set_params",
        "vqf_float_finite",
        "vqf_vec3_finite",
        "vqf_update_gyro",
        "vqf_update_accel",
        "vqf_update_mag",
    ]
    .into_iter()
    .enumerate()
    {
        if index != 0 {
            source.push('\n');
        }
        source.push_str(extract(&wrapper, name)?);
    }
    for name in [
        "sensor_tcal_cache_select_slot",
        "sensor_tcal_mls_lookup_locked",
    ] {
        source.push('\n');
        source.push_str(extract(&thermal, name)?);
    }
    source.push_str(WRAPPER);
    Ok(source)
}

fn compiler_recipe(
    compiler: &OsStr,
    firmware: &Path,
    cmsis_dsp: Option<&Path>,
    build: &Path,
) -> Vec<OsString> {
    let engine_dir = firmware.join("vqf-c/src");
    let mut args = vec![compiler.to_os_string()];
    if let Some(cmsis_dsp) = cmsis_dsp {
        // This is the SDK's historical host sqrtf branch; it does not invoke Python.
        args.extend([
            OsString::from("-D__GNUC_PYTHON__"),
            OsString::from("-DDISABLEFLOAT16"),
            OsString::from("-DCONFIG_CMSIS_DSP=1"),
            OsString::from("-I"),
            cmsis_dsp.join("Include").into_os_string(),
        ]);
        args.extend(
            CMSIS_SOURCE_NAMES
                .iter()
                .map(|name| cmsis_dsp.join("Source").join(name).into_os_string()),
        );
    }
    args.extend([
        OsString::from("-std=c17"),
        OsString::from("-O2"),
        OsString::from("-ffp-contract=off"),
        OsString::from("-shared"),
        OsString::from("-fPIC"),
        OsString::from("-I"),
        engine_dir.as_os_str().to_os_string(),
        build.join("oracle.c").into_os_string(),
        engine_dir.join("vqf.c").into_os_string(),
        OsString::from("-lm"),
        OsString::from("-o"),
        build.join("oracle.so").into_os_string(),
    ]);
    args
}

#[derive(Serialize)]
struct CompilerInvocation {
    format_version: u8,
    representation: &'static str,
    logical_roots: BTreeMap<&'static str, &'static str>,
    arguments: Vec<String>,
}

#[derive(Serialize)]
struct Provenance {
    scope: String,
    compiler_invocation: CompilerInvocation,
    sha256: BTreeMap<String, String>,
    trace_sha256: String,
    cases: [&'static str; 3],
    qualification: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    cmsis_sha256: Option<BTreeMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cmsis_license: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    comparison: Option<trace::Comparison>,
}

fn file_sha256(path: &Path) -> Result<String, String> {
    let mut input =
        File::open(path).map_err(|error| format!("reading {}: {error}", path.display()))?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = input
            .read(&mut buffer)
            .map_err(|error| format!("reading {}: {error}", path.display()))?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(crate::lower_hex(&hash.finalize()))
}

fn generate(options: &Options) -> Result<(), String> {
    let source = wrapper_source(&options.firmware)?;
    {
        let build = tempfile::Builder::new()
            .prefix("slime-reference-")
            .tempdir()
            .map_err(|error| format!("creating temporary oracle directory: {error}"))?;
        let source_path = build.path().join("oracle.c");
        fs::write(&source_path, source)
            .map_err(|error| format!("writing {}: {error}", source_path.display()))?;
        let command = compiler_recipe(
            OsStr::new("gcc"),
            &options.firmware,
            options.cmsis_dsp.as_deref(),
            build.path(),
        );
        let status = Command::new(&command[0])
            .args(&command[1..])
            .status()
            .map_err(|error| format!("starting gcc: {error}"))?;
        if !status.success() {
            return Err(format!("gcc failed with {status}"));
        }
        let mut oracle = ffi::Oracle::load(&build.path().join("oracle.so"))?;
        trace::write(&options.output, &mut oracle)?;
    }

    let mut logical_roots = BTreeMap::from([
        (
            "${COMPILER}",
            "Host GCC C compiler executable; machine location omitted",
        ),
        (
            "${FIRMWARE}",
            "--firmware source root; machine location omitted",
        ),
        (
            "${BUILD}",
            "Temporary oracle source/shared-library directory; machine location omitted",
        ),
    ]);
    if options.cmsis_dsp.is_some() {
        logical_roots.insert(
            "${CMSIS_DSP}",
            "--cmsis-dsp source root; machine location omitted",
        );
    }
    let logical_arguments = compiler_recipe(
        OsStr::new("${COMPILER}"),
        Path::new("${FIRMWARE}"),
        options
            .cmsis_dsp
            .as_ref()
            .map(|_| Path::new("${CMSIS_DSP}")),
        Path::new("${BUILD}"),
    )
    .iter()
    .map(|arg| arg.to_string_lossy().replace('\\', "/"))
    .collect();
    let sha256 = FIRMWARE_SOURCES
        .iter()
        .map(|name| {
            file_sha256(&options.firmware.join(name)).map(|hash| ((*name).to_owned(), hash))
        })
        .collect::<Result<BTreeMap<_, _>, _>>()?;
    let cmsis_sha256 = options
        .cmsis_dsp
        .as_ref()
        .map(|root| {
            CMSIS_SOURCE_NAMES
                .iter()
                .map(|name| {
                    let relative = format!("Source/{name}");
                    file_sha256(&root.join(&relative)).map(|hash| (relative, hash))
                })
                .collect::<Result<BTreeMap<_, _>, _>>()
        })
        .transpose()?;
    let comparison = options
        .compare
        .as_ref()
        .map(|baseline| trace::compare(baseline, &options.output))
        .transpose()?;
    if let Some(comparison) = &comparison {
        println!(
            "{}",
            serde_json::to_string_pretty(comparison).map_err(|error| error.to_string())?
        );
    }
    let provenance = Provenance {
        scope: format!("{} actual firmware wrapper + actual engine + actual MLS fallback; no replay bridge", if options.cmsis_dsp.is_some() { "controlled scalar CMSIS" } else { "portable" }),
        compiler_invocation: CompilerInvocation {
            format_version: 1,
            representation: "Logical-root argument template, not the verbatim executed command; no machine root locations are recorded",
            logical_roots,
            arguments: logical_arguments,
        },
        sha256,
        trace_sha256: file_sha256(&options.output)?,
        cases: [
            "near-inverted acceleration",
            "rest/bias then motion, repeated versus absent acceleration, variable mag elapsed dt",
            "signed negative thermal MLS/cache with sensitivity after subtraction",
        ],
        qualification: "CMSIS target objects are actually compiled -Ofast whereas engine is -Os/-flto. Host scalar -O2/-ffp-contract=off is a controlled algorithm comparison, not exact ARM compiler/FPU equivalence or a worst-case error bound.",
        cmsis_license: cmsis_sha256.as_ref().map(|_| "External SDK Apache-2.0 sources linked into temporary oracle independently of the native subset."),
        cmsis_sha256,
        comparison,
    };
    let provenance_path = options.output.with_extension("provenance.json");
    let contents = serde_json::to_string_pretty(&provenance).map_err(|error| error.to_string())?;
    fs::write(&provenance_path, contents)
        .map_err(|error| format!("writing {}: {error}", provenance_path.display()))?;
    println!("{}", options.output.display());
    Ok(())
}
