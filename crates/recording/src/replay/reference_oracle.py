#!/usr/bin/env python3
"""Generate independent golden traces from the actual reference firmware, never the replay bridge.

Run (WSL, from the repository root):
  python3 crates/recording/src/replay/reference_oracle.py \\
    --firmware "$FIRMWARE_ROOT" --cmsis-dsp "$CMSIS_DSP_ROOT" --output ./replay-oracle.csv
Then Windows: $env:SLIME_REPLAY_ORACLE='./replay-oracle.csv'; cargo test -p slimenrf-recording replay::tests::firmware_reference_trace -- --ignored
Omit --cmsis-dsp for an independent portable baseline; pass --compare baseline.csv to quantify its difference.
Scalar CMSIS algorithm parity only, not exact target compiler/FPU or live retained-state equality.
Published provenance uses logical-root compiler arguments, not a verbatim command or machine paths.
The comparison baseline is identified by role and content SHA-256, never its local filename.
"""
import argparse
import ctypes as C
import csv
import hashlib
import json
from pathlib import Path, PurePosixPath
import re
import subprocess
import tempfile


_CMSIS_SOURCE_NAMES = (
    'ControllerFunctions/arm_sin_cos_f32.c', 'FastMathFunctions/arm_atan2_f32.c',
    'BasicMathFunctions/arm_dot_prod_f32.c', 'BasicMathFunctions/arm_scale_f32.c',
    'BasicMathFunctions/arm_clip_f32.c', 'SupportFunctions/arm_fill_f32.c',
    'CommonTables/arm_common_tables.c')


def _compiler_command(compiler, firmware, cmsis_dsp, build):
    # Build actual and published arguments from the same recipe. The published
    # call supplies only logical roots, including for an absolute compiler path.
    engine = firmware / 'vqf-c/src/vqf.c'
    command = [str(compiler),'-std=c17','-O2','-ffp-contract=off','-shared','-fPIC',
               '-I',str(engine.parent),str(build/'oracle.c'),str(engine),'-lm','-o',str(build/'oracle.so')]
    if cmsis_dsp:
        command[1:1] = ['-D__GNUC_PYTHON__','-DDISABLEFLOAT16','-DCONFIG_CMSIS_DSP=1',
                        '-I',str(cmsis_dsp/'Include'),
                        *(str(cmsis_dsp/'Source'/name) for name in _CMSIS_SOURCE_NAMES)]
    return command


def extract(source, name):
    match = re.search(r'^(?:static )?(?:ALWAYS_INLINE )?(?:void|bool|float|int) ' + re.escape(name) + r'\([^;{]*\)\s*\{', source, re.M)
    if not match:
        raise RuntimeError('Missing production function: ' + name)
    start = match.start()
    pos = match.end()
    depth = 1
    # Production extracted functions have no brace-bearing string literals.
    while depth:
        if source[pos] == '{': depth += 1
        elif source[pos] == '}': depth -= 1
        pos += 1
    return source[start:pos]


import math

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--firmware', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--cmsis-dsp', type=Path, help='Exact SDK modules/lib/cmsis-dsp root; enables controlled scalar CMSIS algorithm oracle')
    parser.add_argument('--compare', type=Path, help='Portable oracle CSV to quantify CMSIS differences; not a universal error bound')
    args = parser.parse_args()
    wrapper_path = args.firmware / 'src/sensor/fusion/vqf/vqf.c'
    thermal_path = args.firmware / 'src/sensor/calibration/tcal_mls_lut.c'
    engine_path = args.firmware / 'vqf-c/src/vqf.c'
    wrapper = wrapper_path.read_text()
    thermal = thermal_path.read_text()
    cmsis_sources = []
    if args.cmsis_dsp:
        cmsis_sources = [args.cmsis_dsp / 'Source' / name for name in _CMSIS_SOURCE_NAMES]
    preamble = r'''
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
'''
    source = preamble + '\n'.join(extract(wrapper, name) for name in (
        'set_params', 'vqf_float_finite', 'vqf_vec3_finite', 'vqf_update_gyro', 'vqf_update_accel', 'vqf_update_mag'))
    source += '\n' + extract(thermal, 'sensor_tcal_cache_select_slot')
    source += '\n' + extract(thermal, 'sensor_tcal_mls_lookup_locked')
    source += r'''
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
'''
    with tempfile.TemporaryDirectory(prefix='slime-reference-') as temporary:
        root = Path(temporary)
        (root / 'oracle.c').write_text(source)
        command = _compiler_command('gcc', args.firmware, args.cmsis_dsp, root)
        subprocess.run(command,check=True)
        lib = C.CDLL(str(root/'oracle.so'))
        ptr = C.POINTER(C.c_float)
        lib.oracle_init.argtypes = []
        lib.oracle_step.argtypes = [ptr,ptr,ptr,C.c_float,C.c_float,C.c_int,ptr]
        vector = C.c_float * 3
        output = C.c_float * 22
        args.output.parent.mkdir(parents=True,exist_ok=True)
        with args.output.open('w',newline='') as handle:
            writer=csv.writer(handle)
            writer.writerow(['case','index','gx','gy','gz','ax','ay','az','mx','my','mz','temperature']+[f's{i}' for i in range(22)])
            for case in range(3):
                lib.oracle_init()
                last_mag=None
                for i in range(2400):
                    if case==0:
                        gyro=[0.,0.,0.];acc=[0.000001,0.,-1.]
                    elif case==1:
                        gyro=[.12,-.21,.3] if i<1600 else [20.,5.,-10.]
                        acc=[0.,0.,1.] if i%11!=0 else [0.,0.,0.]
                    else:
                        gyro=[1.,-2.,3.];acc=[0.,0.,1.]
                    mag=[.1,.3,-.2] if case==1 and i%7 in (0,2) else [0.,0.,0.]
                    temperature=-15.+(i%90)*.01
                    dt=.02 if last_mag is None else (i-last_mag)*.01
                    if any(mag): last_mag=i
                    result=output()
                    lib.oracle_step(vector(*gyro),vector(*acc),vector(*mag),dt,temperature,case==2,result)
                    writer.writerow([case,i,*gyro,*acc,*mag,temperature,*result])
    logical_roots = {
        '${COMPILER}': 'Host GCC C compiler executable; machine location omitted',
        '${FIRMWARE}': '--firmware source root; machine location omitted',
        '${BUILD}': 'Temporary oracle source/shared-library directory; machine location omitted'}
    if args.cmsis_dsp:
        logical_roots['${CMSIS_DSP}'] = '--cmsis-dsp source root; machine location omitted'
    provenance={'scope':('controlled scalar CMSIS' if args.cmsis_dsp else 'portable')+' actual firmware wrapper + actual engine + actual MLS fallback; no replay bridge',
        'compiler_invocation':{
            'format_version':1,
            'representation':'Logical-root argument template, not the verbatim executed command; no machine root locations are recorded',
            'logical_roots':logical_roots,
            'arguments':_compiler_command('${COMPILER}', PurePosixPath('${FIRMWARE}'),
                PurePosixPath('${CMSIS_DSP}') if args.cmsis_dsp else None, PurePosixPath('${BUILD}'))},
        'sha256':{p.relative_to(args.firmware).as_posix():hashlib.sha256(p.read_bytes()).hexdigest() for p in (wrapper_path,thermal_path,engine_path)},
        'trace_sha256':hashlib.sha256(args.output.read_bytes()).hexdigest(),
        'cases':['near-inverted acceleration','rest/bias then motion, repeated versus absent acceleration, variable mag elapsed dt','signed negative thermal MLS/cache with sensitivity after subtraction'],
        'qualification':'CMSIS target objects are actually compiled -Ofast whereas engine is -Os/-flto. Host scalar -O2/-ffp-contract=off is a controlled algorithm comparison, not exact ARM compiler/FPU equivalence or a worst-case error bound.'}
    if cmsis_sources:
        provenance['cmsis_sha256']={p.relative_to(args.cmsis_dsp).as_posix():hashlib.sha256(p.read_bytes()).hexdigest() for p in cmsis_sources}
        provenance['cmsis_license']='External SDK Apache-2.0 sources linked into temporary oracle independently of the native subset.'
    if args.compare:
        with args.compare.open(newline='') as a, args.output.open(newline='') as b:
            baseline=list(csv.reader(a))[1:]; measured=list(csv.reader(b))[1:]
        if len(baseline)!=len(measured): raise RuntimeError('Comparison trace length differs')
        maxima=[0.0]*22
        pose_max=[0.0,0.0]
        state_mismatch=[0,0]
        for left,right in zip(baseline,measured):
            if left[:12]!=right[:12]: raise RuntimeError('Comparison input events differ')
            left=list(map(float,left[12:]));right=list(map(float,right[12:]))
            for i,(x,y) in enumerate(zip(left,right)):
                if math.isfinite(x) and math.isfinite(y): maxima[i]=max(maxima[i],abs(x-y))
                elif not(math.isnan(x) and math.isnan(y)): raise RuntimeError('Nonfinite comparison mismatch')
            for i,start in enumerate((0,4)):
                q=left[start:start+4];r=right[start:start+4]
                dot=abs(sum(x*y for x,y in zip(q,r)))/math.sqrt(sum(x*x for x in q)*sum(x*x for x in r))
                pose_max[i]=max(pose_max[i],math.degrees(2*math.acos(min(1.,dot))))
            for i,channel in enumerate((12,13)): state_mismatch[i]+=left[channel]!=right[channel]
        provenance['comparison']={'baseline':{'logical_id':'comparison-baseline','sha256':hashlib.sha256(args.compare.read_bytes()).hexdigest()},'rows':len(baseline),'max_abs_s0_through_s21':maxima,'max_pose_degrees_6d_9d':pose_max,'rest_magdist_mismatches':state_mismatch}
        print(json.dumps(provenance['comparison'],indent=2))
    args.output.with_suffix('.provenance.json').write_text(json.dumps(provenance,indent=2))
    print(args.output)

if __name__=='__main__':main()
