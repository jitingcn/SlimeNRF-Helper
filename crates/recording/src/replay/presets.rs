//! Web-compatible parameter files and bounded, transactional custom presets.
use super::ReplayParams;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de, ser::SerializeMap};
use std::{
    fmt, fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

const PARAMETER_COUNT: usize = 25;
const MAX_YAML_BYTES: usize = 64 * 1024;
const MAX_STORE_BYTES: usize = 1024 * 1024;
const MAX_PRESETS: usize = 128;

// One mapping owns both YAML spelling and full persistence/export serialization.
macro_rules! parameter_fields {
    ($consumer:ident $(, $arg:ident)*) => {
        $consumer! { $($arg,)*
            numbers {
                "tauAcc" => tau_acc, "tauMag" => tau_mag,
                "biasClip" => bias_clip, "biasSigmaInit" => bias_sigma_init,
                "biasForgettingTime" => bias_forgetting_time,
                "biasSigmaMotion" => bias_sigma_motion, "biasSigmaRest" => bias_sigma_rest,
                "biasVerticalForgettingFactor" => bias_vertical_forgetting_factor,
                "restMinT" => rest_min_t, "restFilterTau" => rest_filter_tau,
                "restThGyr" => rest_th_gyr, "restThAcc" => rest_th_acc,
                "magCurrentTau" => mag_current_tau, "magRefTau" => mag_ref_tau,
                "magNormTh" => mag_norm_th, "magDipTh" => mag_dip_th,
                "magNewTime" => mag_new_time, "magNewFirstTime" => mag_new_first_time,
                "magNewMinGyr" => mag_new_min_gyr,
                "magMinUndisturbedTime" => mag_min_undisturbed_time,
                "magMaxRejectionTime" => mag_max_rejection_time,
                "magRejectionFactor" => mag_rejection_factor
            }
            toggles {
                "motionBiasEstEnabled" => motion_bias_est_enabled,
                "restBiasEstEnabled" => rest_bias_est_enabled,
                "magDistRejectionEnabled" => mag_dist_rejection_enabled
            }
        }
    };
}
macro_rules! define_patch {
    (numbers {$($key:literal => $field:ident),*}
     toggles {$($toggle:literal => $flag:ident),*}) => {
        struct ParameterName(&'static str);
        impl<'de> Deserialize<'de> for ParameterName {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                struct NameVisitor;
                impl<'de> de::Visitor<'de> for NameVisitor {
                    type Value = ParameterName;
                    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                        formatter.write_str("a VQF parameter name")
                    }
                    fn visit_str<E: de::Error>(self, name: &str) -> Result<Self::Value, E> {
                        match name {
                            $($key => Ok(ParameterName($key)),)*
                            $($toggle => Ok(ParameterName($toggle)),)*
                            _ => Err(E::custom(format!("Unknown VQF parameter: {name}"))),
                        }
                    }
                }
                // deserialize_str would also coerce YAML boolean/numeric keys.
                deserializer.deserialize_any(NameVisitor)
            }
        }

        #[derive(Default)]
        struct Patch {
            $($field: Option<FiniteF32>,)*
            $($flag: Option<bool>,)*
            len: usize,
        }
        impl Patch {
            fn read_field<'de, M: de::MapAccess<'de>>(
                &mut self,
                name: ParameterName,
                map: &mut M,
            ) -> Result<(), M::Error> {
                match name.0 {
                    $($key => read_field(&mut self.$field, name.0, map, &mut self.len),)*
                    $($toggle => read_field(&mut self.$flag, name.0, map, &mut self.len),)*
                    _ => Err(de::Error::custom(format!("Unknown VQF parameter: {}", name.0))),
                }
            }
            fn merge(self, base: &ReplayParams) -> Result<ReplayParams, String> {
                let mut params = base.clone();
                $(if let Some(value) = self.$field {
                    params.$field = value.0;
                })*
                $(if let Some(value) = self.$flag {
                    params.$flag = value;
                })*
                params.validate()?;
                Ok(params)
            }
        }
    };
}
macro_rules! serialize_fields {
    ($params:ident, $map:ident,
     numbers {$($key:literal => $field:ident),*}
     toggles {$($toggle:literal => $flag:ident),*}) => {
        $( $map.serialize_entry($key, &$params.$field)?; )*
        $( $map.serialize_entry($toggle, &$params.$flag)?; )*
    };
}

// Scalar visitors never descend into a sequence or mapping, including alias targets.
struct FiniteF32(f32);
impl<'de> Deserialize<'de> for FiniteF32 {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct NumberVisitor;
        impl<'de> de::Visitor<'de> for NumberVisitor {
            type Value = FiniteF32;
            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a finite 32-bit number")
            }
            fn visit_i64<E: de::Error>(self, value: i64) -> Result<Self::Value, E> {
                self.visit_f64(value as f64)
            }
            fn visit_u64<E: de::Error>(self, value: u64) -> Result<Self::Value, E> {
                self.visit_f64(value as f64)
            }
            fn visit_f64<E: de::Error>(self, value: f64) -> Result<Self::Value, E> {
                let narrowed = value as f32;
                if !value.is_finite() || !narrowed.is_finite() {
                    return Err(E::custom("expected a finite 32-bit number"));
                }
                Ok(FiniteF32(narrowed))
            }
        }
        deserializer.deserialize_any(NumberVisitor)
    }
}

parameter_fields!(define_patch);

fn read_field<'de, T: Deserialize<'de>, M: de::MapAccess<'de>>(
    slot: &mut Option<T>,
    name: &str,
    map: &mut M,
    len: &mut usize,
) -> Result<(), M::Error> {
    if slot.is_some() {
        return Err(de::Error::custom(format!(
            "Duplicate VQF parameter: {name}"
        )));
    }
    if *len == PARAMETER_COUNT {
        return Err(de::Error::custom("Too many VQF parameters"));
    }
    *slot = Some(map.next_value()?);
    *len += 1;
    Ok(())
}
impl<'de> Deserialize<'de> for Patch {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct MappingVisitor;
        impl<'de> de::Visitor<'de> for MappingVisitor {
            type Value = Patch;
            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a non-empty flat VQF parameter mapping")
            }
            fn visit_map<M: de::MapAccess<'de>>(self, mut map: M) -> Result<Patch, M::Error> {
                let mut patch = Patch::default();
                while let Some(name) = map.next_key::<ParameterName>()? {
                    // A recognized scalar key is required before reading its value.
                    patch.read_field(name, &mut map)?;
                }
                if patch.len == 0 {
                    return Err(de::Error::custom(
                        "Expected a non-empty flat VQF parameter mapping",
                    ));
                }
                Ok(patch)
            }
        }
        deserializer.deserialize_map(MappingVisitor)
    }
}

/// Parse all input and validate a clone before returning; the original never changes.
/// Names and boolean scalars follow the web analyzer's YAML 1.2 convention.
pub fn merge_yaml(base: &ReplayParams, yaml: &str) -> Result<ReplayParams, String> {
    if yaml.len() > MAX_YAML_BYTES {
        return Err("VQF parameter YAML exceeds 64 KiB".into());
    }
    let patch = serde_yaml::from_str::<Patch>(yaml)
        .map_err(|error| format!("Invalid VQF parameter YAML: {error}"))?;
    reject_yaml_indirections(yaml)?;
    patch.merge(base)
}

// Run only after the flat typed schema succeeds: admitted names contain no '#',
// and numeric/boolean values cannot contain one either. Every raw '#' is therefore
// a comment (or occurs after a tag/anchor marker that is itself rejected). Quotes
// need no lexical interpretation and cannot hide indirections in plain scalars.
// Reject aliases even when their primitive targets would otherwise be valid.
fn reject_yaml_indirections(yaml: &str) -> Result<(), String> {
    for line in yaml.split(['\n', '\r', '\u{85}', '\u{2028}', '\u{2029}']) {
        let content = line.split_once('#').map_or(line, |(content, _)| content);
        if content.contains(['&', '*', '!']) {
            return Err(
                "VQF parameter YAML does not accept anchors, aliases or explicit tags".into(),
            );
        }
    }
    Ok(())
}

struct ParameterDocument<'a>(&'a ReplayParams);
impl Serialize for ParameterDocument<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let params = self.0;
        params.validate().map_err(serde::ser::Error::custom)?;
        let mut map = serializer.serialize_map(Some(PARAMETER_COUNT))?;
        parameter_fields!(serialize_fields, params, map);
        map.end()
    }
}
/// Export all 25 parameters, including actual booleans (not numeric toggles).
pub fn to_yaml(params: &ReplayParams) -> Result<String, String> {
    serde_yaml::to_string(&ParameterDocument(params))
        .map(|yaml| format!("# VQF parameter configuration\n{yaml}"))
        .map_err(|error| format!("Cannot export VQF parameters: {error}"))
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preset {
    pub name: String,
    #[serde(
        serialize_with = "serialize_params",
        deserialize_with = "deserialize_params"
    )]
    pub params: ReplayParams,
}
fn serialize_params<S: Serializer>(
    params: &ReplayParams,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    ParameterDocument(params).serialize(serializer)
}
fn deserialize_params<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<ReplayParams, D::Error> {
    let patch = Patch::deserialize(deserializer)?;
    if patch.len != PARAMETER_COUNT {
        return Err(de::Error::custom(
            "Stored presets must contain all 25 parameters",
        ));
    }
    patch
        .merge(&ReplayParams::default())
        .map_err(de::Error::custom)
}

fn c_defaults() -> ReplayParams {
    ReplayParams {
        tau_acc: 3.,
        tau_mag: 9.,
        motion_bias_est_enabled: true,
        rest_bias_est_enabled: true,
        mag_dist_rejection_enabled: true,
        bias_sigma_init: 0.5,
        bias_forgetting_time: 100.,
        bias_clip: 2.,
        bias_sigma_motion: 0.1,
        bias_vertical_forgetting_factor: 0.0001,
        bias_sigma_rest: 0.03,
        rest_min_t: 1.5,
        rest_filter_tau: 0.5,
        rest_th_gyr: 2.,
        rest_th_acc: 0.5,
        mag_current_tau: 0.05,
        mag_ref_tau: 20.,
        mag_norm_th: 0.1,
        mag_dip_th: 10.,
        mag_new_time: 20.,
        mag_new_first_time: 5.,
        mag_new_min_gyr: 20.,
        mag_min_undisturbed_time: 0.5,
        mag_max_rejection_time: 60.,
        mag_rejection_factor: 2.,
    }
}
fn web_development() -> ReplayParams {
    ReplayParams {
        tau_acc: 3.8,
        tau_mag: 9.,
        motion_bias_est_enabled: true,
        rest_bias_est_enabled: true,
        mag_dist_rejection_enabled: true,
        bias_sigma_init: 1.,
        bias_forgetting_time: 100.,
        bias_clip: 5.,
        bias_sigma_motion: 0.28,
        bias_vertical_forgetting_factor: 0.0001,
        bias_sigma_rest: 0.05,
        rest_min_t: 2.8,
        rest_filter_tau: 1.34,
        rest_th_gyr: 0.8,
        rest_th_acc: 0.08,
        mag_current_tau: 0.50,
        mag_ref_tau: 10.,
        mag_norm_th: 0.10,
        mag_dip_th: 4.,
        mag_new_time: 3.,
        mag_new_first_time: 3.,
        mag_new_min_gyr: 20.,
        mag_min_undisturbed_time: 0.5,
        mag_max_rejection_time: 3200.,
        mag_rejection_factor: 1150.,
    }
}

/// Source: web AnalysisPanel.tsx and lib/vqf/firmware-presets.ts.
/// Sparse historical tags inherit VQF-C defaults, as wasm/vqf_wasm.c does via
/// init_params before applying the tag's overrides, never the development profile.
/// A web tag is a source profile, not detection of the connected device's firmware.
pub fn builtin_presets() -> Vec<Preset> {
    let development = web_development();
    let tag_260824 = ReplayParams {
        bias_sigma_init: 0.8,
        ..development.clone()
    };
    let tag_260708 = ReplayParams {
        tau_acc: 4.3,
        bias_clip: 2.,
        bias_forgetting_time: 427.,
        bias_sigma_init: 1.10,
        bias_sigma_motion: 0.048,
        bias_sigma_rest: 0.0153,
        rest_filter_tau: 0.66,
        rest_min_t: 0.63,
        rest_th_gyr: 0.68,
        rest_th_acc: 0.21,
        tau_mag: 6.,
        mag_current_tau: 0.50,
        mag_norm_th: 0.08,
        mag_dip_th: 6.,
        mag_ref_tau: 15.,
        mag_new_time: 12.,
        mag_new_first_time: 5.5,
        mag_new_min_gyr: 16.,
        mag_min_undisturbed_time: 1.,
        mag_max_rejection_time: 3200.,
        mag_rejection_factor: 1150.,
        ..c_defaults()
    };
    let tag_260513 = ReplayParams {
        tau_mag: 9.,
        mag_norm_th: 0.09,
        mag_min_undisturbed_time: 0.6,
        ..tag_260708.clone()
    };
    let tag_260402 = ReplayParams {
        tau_acc: 3.2,
        bias_clip: 2.5,
        bias_forgetting_time: 130.,
        bias_sigma_init: 1.2,
        bias_sigma_motion: 0.20,
        bias_sigma_rest: 0.03,
        bias_vertical_forgetting_factor: 0.00001,
        rest_filter_tau: 1.3,
        rest_min_t: 2.5,
        rest_th_acc: 0.20,
        rest_th_gyr: 0.55,
        tau_mag: 12.,
        mag_current_tau: 0.20,
        mag_norm_th: 0.09,
        mag_dip_th: 9.,
        mag_new_first_time: 6.,
        mag_new_min_gyr: 15.,
        mag_min_undisturbed_time: 0.8,
        mag_max_rejection_time: 1800.,
        mag_rejection_factor: 900.,
        ..c_defaults()
    };
    let tag_260301 = ReplayParams {
        bias_clip: 2.5,
        tau_mag: 10.,
        bias_forgetting_time: 120.,
        bias_sigma_init: 2.,
        bias_sigma_motion: 0.20,
        bias_sigma_rest: 0.03,
        bias_vertical_forgetting_factor: 0.00001,
        rest_filter_tau: 1.4,
        rest_min_t: 2.8,
        rest_th_acc: 0.25,
        rest_th_gyr: 0.8,
        tau_acc: 3.6,
        ..c_defaults()
    };
    [
        (
            "Native firmware baseline · 8aa61a6",
            ReplayParams::default(),
        ),
        ("VQF-C defaults", c_defaults()),
        ("Web development profile", development),
        ("Web firmware tag 260824", tag_260824),
        ("Web firmware tag 260708", tag_260708),
        ("Web firmware tag 260513", tag_260513),
        ("Web firmware tag 260402", tag_260402),
        ("Web firmware tag 260301", tag_260301),
    ]
    .into_iter()
    .map(|(name, params)| Preset {
        name: name.into(),
        params,
    })
    .collect()
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoreDocument {
    version: u32,
    presets: Vec<Preset>,
}

/// A successfully loaded custom store. There is intentionally no Default that
/// could turn a damaged/unreadable file into permission to overwrite it.
#[derive(Debug)]
pub struct PresetStore {
    path: PathBuf,
    presets: Vec<Preset>,
}
impl PresetStore {
    pub fn load(path: Option<PathBuf>) -> Result<Self, String> {
        let path = path.ok_or_else(|| "The user settings directory is unavailable.".to_owned())?;
        Self::load_from(path)
    }
    fn load_from(path: PathBuf) -> Result<Self, String> {
        let presets = read_store(&path)?;
        Ok(Self { path, presets })
    }
    pub fn presets(&self) -> &[Preset] {
        &self.presets
    }

    /// Create or explicitly overwrite a custom preset with the same name.
    pub fn save(&mut self, name: &str, params: &ReplayParams) -> Result<(), String> {
        let name = name.trim();
        validate_name(name)?;
        params.validate()?;
        let mut next = read_store(&self.path)?;
        if let Some(existing) = next.iter_mut().find(|preset| preset.name == name) {
            existing.params = params.clone();
        } else {
            if next.len() == MAX_PRESETS {
                return Err(format!(
                    "At most {MAX_PRESETS} custom VQF presets can be saved"
                ));
            }
            next.push(Preset {
                name: name.into(),
                params: params.clone(),
            });
        }
        self.commit(next)
    }
    pub fn rename(&mut self, old: &str, new: &str) -> Result<(), String> {
        let new = new.trim();
        validate_name(new)?;
        let mut next = read_store(&self.path)?;
        let index = next
            .iter()
            .position(|preset| preset.name == old)
            .ok_or_else(|| {
                format!("Custom VQF preset '{old}' no longer exists; reload the presets")
            })?;
        if old != new && next.iter().any(|preset| preset.name == new) {
            return Err(format!("A custom VQF preset named '{new}' already exists"));
        }
        next[index].name = new.into();
        self.commit(next)
    }
    pub fn delete(&mut self, name: &str) -> Result<(), String> {
        let mut next = read_store(&self.path)?;
        let index = next
            .iter()
            .position(|preset| preset.name == name)
            .ok_or_else(|| {
                format!("Custom VQF preset '{name}' no longer exists; reload the presets")
            })?;
        next.remove(index);
        self.commit(next)
    }
    fn commit(&mut self, presets: Vec<Preset>) -> Result<(), String> {
        // Each operation reloaded and validated the file before preparing this edit.
        let document = StoreDocument {
            version: 1,
            presets,
        };
        let bytes = serde_json::to_vec_pretty(&document).map_err(|error| error.to_string())?;
        if bytes.len() + 1 > MAX_STORE_BYTES {
            return Err("Custom VQF presets exceed the 1 MiB storage limit".into());
        }
        let parent = self
            .path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let persist = || -> Result<(), Box<dyn std::error::Error>> {
            fs::create_dir_all(parent)?;
            let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
            temporary.write_all(&bytes)?;
            temporary.write_all(b"\n")?;
            temporary.as_file().sync_all()?;
            temporary.persist(&self.path)?;
            Ok(())
        };
        persist().map_err(|error| {
            format!(
                "Cannot save VQF presets at {}: {error}. Existing presets were not replaced.",
                self.path.display()
            )
        })?;
        self.presets = document.presets;
        Ok(())
    }
}
fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty()
        || name.len() > 120
        || name.trim() != name
        || name.chars().any(char::is_control)
    {
        return Err("Preset names must contain 1–120 UTF-8 bytes, without surrounding whitespace or control characters".into());
    }
    Ok(())
}
fn read_store(path: &Path) -> Result<Vec<Preset>, String> {
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(format!(
                "Cannot read VQF presets at {}: {error}",
                path.display()
            ));
        }
    };
    let invalid = |error: String| {
        format!(
            "VQF presets at {} are invalid and were not overwritten: {error}. Restore or move this file before saving presets.",
            path.display()
        )
    };
    let metadata = file
        .metadata()
        .map_err(|error| invalid(error.to_string()))?;
    if !metadata.is_file() || metadata.len() > MAX_STORE_BYTES as u64 {
        return Err(invalid(
            "Expected a regular preset file no larger than 1 MiB".into(),
        ));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_STORE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| invalid(error.to_string()))?;
    if bytes.len() > MAX_STORE_BYTES {
        return Err(invalid("Preset file exceeds 1 MiB".into()));
    }
    let document: StoreDocument =
        serde_json::from_slice(&bytes).map_err(|error| invalid(error.to_string()))?;
    if document.version != 1 {
        return Err(invalid(format!(
            "Unsupported store version {}",
            document.version
        )));
    }
    if document.presets.len() > MAX_PRESETS {
        return Err(invalid(format!("More than {MAX_PRESETS} presets")));
    }
    for (index, preset) in document.presets.iter().enumerate() {
        validate_name(&preset.name).map_err(invalid)?;
        if document.presets[..index]
            .iter()
            .any(|old| old.name == preset.name)
        {
            return Err(invalid(format!(
                "Duplicate custom preset name '{}'",
                preset.name
            )));
        }
    }
    Ok(document.presets)
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_STORE_BYTES, MAX_YAML_BYTES, PresetStore, builtin_presets, c_defaults, merge_yaml,
        to_yaml,
    };
    use crate::replay::ReplayParams;
    use std::fs;

    #[test]
    fn partial_yaml_merges_without_mutating_source() {
        let base = ReplayParams::default();
        let merged = merge_yaml(&base, "tauAcc: 3.8\nrestBiasEstEnabled: false\n").unwrap();
        assert_eq!(base, ReplayParams::default());
        assert_eq!(merged.tau_acc, 3.8);
        assert!(!merged.rest_bias_est_enabled);
        assert_eq!(merged.mag_rejection_factor, base.mag_rejection_factor);
    }
    #[test]
    fn strict_yaml_rejects_invalid_input_atomically() {
        let base = ReplayParams::default();
        for yaml in [
            "",
            "{}",
            "[]",
            "true",
            "null",
            "tauAcc: 3\ntauAcc: 4",
            "other: 1",
            "restBiasEstEnabled: 1",
            "motionBiasEstEnabled: yes",
            "magDistRejectionEnabled: 'true'",
            "tauAcc: true",
            "tauAcc: '3'",
            "tauAcc: null",
            "tauAcc: .nan",
            "tauAcc: .inf",
            "tauAcc: 1e100",
            "tauAcc: -2",
            "tauAcc: [3]",
            "tauAcc: {value: 3}",
            "tauAcc: 3\nrestMinT: [2]",
            "tauAcc: &a 3\ntauMag: *a",
            "tauAcc: !!float 3",
            "tauAcc: 3\n---\ntauMag: 9",
            "true: 1",
            "[tauAcc]: 3",
            "tauAcc: 3\n<<: {tauMag: 5}",
        ] {
            assert!(merge_yaml(&base, yaml).is_err(), "accepted {yaml:?}");
            assert_eq!(base, ReplayParams::default());
        }
        assert!(merge_yaml(&base, &" ".repeat(MAX_YAML_BYTES + 1)).is_err());
    }

    #[test]
    fn yaml_comments_and_quoted_names_preserve_scalar_types() {
        let base = ReplayParams::default();
        let yaml = r#"
# quotes ' " and indirections &anchor *alias !tag are only comment text
"tauAcc": 3.8 # another ' " &anchor *alias !tag
'tauMag': 9
"restBias\u0045stEnabled": false
motionBiasEstEnabled: true
magDistRejectionEnabled: false
biasSigmaInit: 5e-1
"bias\x43lip": 2.5
magCurrentTau: -1
magNormTh: 1e-100
"#;
        let merged = merge_yaml(&base, yaml).unwrap();
        assert_eq!(merged.tau_acc, 3.8);
        assert_eq!(merged.tau_mag, 9.);
        assert_eq!(merged.bias_sigma_init, 0.5);
        assert_eq!(merged.bias_clip, 2.5);
        assert_eq!(merged.mag_current_tau, -1.);
        assert_eq!(merged.mag_norm_th, 0.);
        assert!(!merged.rest_bias_est_enabled);
        assert!(merged.motion_bias_est_enabled);
        assert!(!merged.mag_dist_rejection_enabled);
        assert_eq!(base, ReplayParams::default());
    }

    #[test]
    fn yaml_indirections_and_embedded_punctuation_are_rejected_atomically() {
        let base = c_defaults();
        let before = base.clone();
        for yaml in [
            "bad#key: &a [1, 2]\ntauAcc: *a",
            "bad'key: &a [1, 2]\ntauAcc: *a",
            "bad\"key: &a [1, 2]\ntauAcc: *a",
            "\"bad # key\": &a [1, 2]\ntauAcc: *a",
            "'bad '' key': &a [1, 2]\ntauAcc: *a",
            "tauAcc: 3#not-a-comment",
            "tauAcc: 3'not-a-quote",
            "tauAcc: 3\"not-a-quote",
            "&root {tauAcc: 3}",
            "&key tauAcc: 3",
            "tauAcc: &number 3",
            "tauAcc: &number 3\ntauMag: *number",
            "restBiasEstEnabled: &flag true\nmotionBiasEstEnabled: *flag",
            "restBiasEstEnabled: !!bool true",
            "tauAcc: !<tag:yaml.org,2002:float> 3",
            "!!map {tauAcc: 3}",
            "!!str tauAcc: 3",
            "tauAcc: 3\n'tauAcc': 4",
            "restBiasEstEnabled: true\n\"restBiasEstEnabled\": false",
            "? {tauAcc: 3}\n: 4",
            "? [[tauAcc]]\n: 4",
            "tauAcc: [[[[3]]]]",
            "restBiasEstEnabled: {nested: {value: true}}",
            "tauAcc: 1000.01",
            "tauAcc: 1e-100",
            "magNormTh: -.inf",
        ] {
            assert!(merge_yaml(&base, yaml).is_err(), "accepted {yaml:?}");
            assert_eq!(base, before, "modified base for {yaml:?}");
        }
    }

    #[test]
    fn unknown_parameter_is_rejected_before_its_value() {
        for yaml in [
            "bad#key: [[[1]]]",
            "bad'key: {nested: [1]}",
            "bad\"key: {nested: [1]}",
            "\"bad # key\": {nested: [1]}",
        ] {
            let error = merge_yaml(&ReplayParams::default(), yaml).unwrap_err();
            assert!(
                error.contains("Unknown VQF parameter:"),
                "unexpected error for {yaml:?}: {error}"
            );
        }
    }

    #[test]
    fn flat_schema_rejects_nested_nodes_without_visiting_their_children() {
        use serde::{
            Deserializer,
            de::{
                self, IntoDeserializer,
                value::{Error, MapDeserializer},
            },
        };

        enum Node {
            Name(&'static str),
            Number(f64),
            Toggle(bool),
            Sequence,
            Mapping,
        }
        struct UnreadChildren;
        impl<'de> de::SeqAccess<'de> for UnreadChildren {
            type Error = Error;
            fn next_element_seed<T: de::DeserializeSeed<'de>>(
                &mut self,
                _seed: T,
            ) -> Result<Option<T::Value>, Error> {
                panic!("flat schema must not visit sequence children")
            }
        }
        impl<'de> de::MapAccess<'de> for UnreadChildren {
            type Error = Error;
            fn next_key_seed<T: de::DeserializeSeed<'de>>(
                &mut self,
                _seed: T,
            ) -> Result<Option<T::Value>, Error> {
                panic!("flat schema must not visit mapping children")
            }
            fn next_value_seed<T: de::DeserializeSeed<'de>>(
                &mut self,
                _seed: T,
            ) -> Result<T::Value, Error> {
                panic!("flat schema must not visit mapping children")
            }
        }
        impl<'de> IntoDeserializer<'de, Error> for Node {
            type Deserializer = Self;
            fn into_deserializer(self) -> Self {
                self
            }
        }
        impl<'de> Deserializer<'de> for Node {
            type Error = Error;
            fn deserialize_any<V: de::Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
                match self {
                    Self::Name(name) => visitor.visit_str(name),
                    Self::Number(value) => visitor.visit_f64(value),
                    Self::Toggle(value) => visitor.visit_bool(value),
                    Self::Sequence => visitor.visit_seq(UnreadChildren),
                    Self::Mapping => visitor.visit_map(UnreadChildren),
                }
            }
            serde::forward_to_deserialize_any! {
                bool i8 i16 i32 i64 u8 u16 u32 u64 f32 f64 char str string
                bytes byte_buf option unit unit_struct newtype_struct seq
                tuple tuple_struct map struct enum identifier ignored_any
            }
        }

        for (key, value) in [
            (Node::Sequence, Node::Name("ignored")),
            (Node::Mapping, Node::Name("ignored")),
            (Node::Name("unknown"), Node::Sequence),
            (Node::Name("tauAcc"), Node::Sequence),
            (Node::Name("tauAcc"), Node::Mapping),
            (Node::Name("restBiasEstEnabled"), Node::Sequence),
            (Node::Name("restBiasEstEnabled"), Node::Mapping),
        ] {
            let mapping = MapDeserializer::<_, Error>::new(std::iter::once((key, value)));
            assert!(<super::Patch as serde::Deserialize>::deserialize(mapping).is_err());
        }
        for (name, first) in [
            ("tauAcc", Node::Number(3.)),
            ("restBiasEstEnabled", Node::Toggle(true)),
        ] {
            let mapping = MapDeserializer::<_, Error>::new(
                [
                    (Node::Name(name), first),
                    (Node::Name(name), Node::Sequence),
                ]
                .into_iter(),
            );
            let error = <super::Patch as serde::Deserialize>::deserialize(mapping)
                .err()
                .expect("duplicate field must be rejected");
            assert!(error.to_string().contains("Duplicate VQF parameter:"));
        }
    }
    #[test]
    fn every_profile_roundtrips_all_twenty_five_fields() {
        for preset in builtin_presets() {
            preset.params.validate().unwrap();
            let yaml = to_yaml(&preset.params).unwrap();
            let map: serde_yaml::Mapping = serde_yaml::from_str(&yaml).unwrap();
            assert_eq!(map.len(), 25);
            assert!(yaml.contains("restBiasEstEnabled: true"));
            assert_eq!(
                merge_yaml(&ReplayParams::default(), &yaml).unwrap(),
                preset.params
            );
        }
        let invalid = ReplayParams {
            tau_acc: f32::NAN,
            ..Default::default()
        };
        assert!(to_yaml(&invalid).is_err());
    }
    #[test]
    fn baseline_is_not_the_web_development_profile_and_sparse_tags_use_c_defaults() {
        let presets = builtin_presets();
        assert_eq!(presets.len(), 8);
        assert_eq!(presets[0].params, ReplayParams::default());
        assert_eq!(presets[0].params.tau_acc, 1.011);
        assert_eq!(presets[2].params.tau_acc, 3.8);
        assert_eq!(presets[2].params.bias_sigma_init, 1.);
        assert_eq!(presets[3].params.bias_sigma_init, 0.8);
        assert_eq!(presets[4].params.mag_min_undisturbed_time, 1.);
        assert_eq!(presets[5].params.mag_min_undisturbed_time, 0.6);
        assert_eq!(presets[6].params.mag_ref_tau, c_defaults().mag_ref_tau);
        assert_eq!(presets[6].params.mag_new_time, c_defaults().mag_new_time);
        assert_eq!(
            presets[7].params.mag_rejection_factor,
            c_defaults().mag_rejection_factor
        );
    }
    #[test]
    fn custom_save_overwrite_rename_delete_survive_reload() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("presets.json");
        let mut store = PresetStore::load_from(path.clone()).unwrap();
        let first = ReplayParams::default();
        store.save(" Custom one ", &first).unwrap();
        assert_eq!(
            PresetStore::load_from(path.clone()).unwrap().presets()[0].params,
            first
        );
        let second = c_defaults();
        store.save("Custom one", &second).unwrap();
        store.rename("Custom one", "Renamed").unwrap();
        let reloaded = PresetStore::load_from(path.clone()).unwrap();
        assert_eq!(reloaded.presets(), store.presets());
        assert_eq!(reloaded.presets()[0].name, "Renamed");
        assert_eq!(reloaded.presets()[0].params, second);
        store.delete("Renamed").unwrap();
        assert!(PresetStore::load_from(path).unwrap().presets().is_empty());
    }
    #[test]
    fn damaged_store_is_never_replaced_even_after_successful_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("presets.json");
        let mut store = PresetStore::load_from(path.clone()).unwrap();
        store.save("Saved", &ReplayParams::default()).unwrap();
        let before = store.presets().to_vec();
        for damaged in [
            "not json",
            "{}",
            "{\"version\":2,\"presets\":[]}",
            "{\"version\":1,\"presets\":[{\"name\":\"Partial\",\"params\":{\"tauAcc\":3}}]}",
        ] {
            fs::write(&path, damaged).unwrap();
            assert!(PresetStore::load_from(path.clone()).is_err());
            assert!(store.save("New", &c_defaults()).is_err());
            assert!(store.rename("Saved", "Other").is_err());
            assert!(store.delete("Saved").is_err());
            assert_eq!(fs::read_to_string(&path).unwrap(), damaged);
            assert_eq!(store.presets(), before);
        }
    }
    #[test]
    fn rejected_names_collisions_and_failed_writes_preserve_memory() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("presets.json");
        let mut store = PresetStore::load_from(path.clone()).unwrap();
        store.save("One", &ReplayParams::default()).unwrap();
        store.save("Two", &c_defaults()).unwrap();
        let before = store.presets().to_vec();
        let bytes = fs::read(&path).unwrap();
        assert!(store.rename("One", "Two").is_err());
        assert!(store.rename("Missing", "New").is_err());
        assert!(store.delete("Missing").is_err());
        for name in ["", "  ", "bad\nname", &"x".repeat(121)] {
            assert!(store.save(name, &c_defaults()).is_err());
        }
        assert_eq!(store.presets(), before);
        assert_eq!(fs::read(&path).unwrap(), bytes);
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(store.save("Three", &c_defaults()).is_err());
        assert_eq!(store.presets(), before);
    }
    #[test]
    fn duplicate_stored_names_parameters_and_oversized_files_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("presets.json");
        let mut store = PresetStore::load_from(path.clone()).unwrap();
        store.save("One", &ReplayParams::default()).unwrap();
        let bytes = fs::read_to_string(&path).unwrap();
        let duplicate_key = bytes.replacen("\"tauAcc\":", "\"tauAcc\": 3.0,\n\"tauAcc\":", 1);
        fs::write(&path, duplicate_key).unwrap();
        assert!(PresetStore::load_from(path.clone()).is_err());
        let mut document: serde_json::Value = serde_json::from_str(&bytes).unwrap();
        let entries = document["presets"].as_array_mut().unwrap();
        entries.push(entries[0].clone());
        fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
        assert!(PresetStore::load_from(path.clone()).is_err());
        fs::write(&path, vec![b' '; MAX_STORE_BYTES + 1]).unwrap();
        assert!(PresetStore::load_from(path.clone()).is_err());
        assert!(store.save("Two", &c_defaults()).is_err());
        assert_eq!(
            fs::metadata(&path).unwrap().len(),
            (MAX_STORE_BYTES + 1) as u64
        );
    }
}
