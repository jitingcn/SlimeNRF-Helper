/* SPDX-License-Identifier: MIT
 * Wrapper profile and update ordering derived from SlimeVR Contributors (2025).
 * See provenance.json and LICENSE-SlimeVR. The kernel itself is unmodified.
 */
#include "vqf.h"
#include <stdlib.h>
#include <string.h>
#include <math.h>

typedef struct {
    vqf_params_t params;
    vqf_state_t state;
    vqf_coeffs_t coeffs;
} replay_filter;

void *slime_vqf_create(const float *p, float gyr_dt, float acc_dt, float mag_dt) {
    replay_filter *f = calloc(1, sizeof(*f));
    if (!f) return NULL;
    init_params(&f->params);
    f->params.tauAcc = p[0];
    f->params.tauMag = p[1];
    f->params.biasClip = p[2];
    f->params.biasSigmaInit = p[3];
    f->params.biasForgettingTime = p[4];
    f->params.biasSigmaMotion = p[5];
    f->params.biasSigmaRest = p[6];
    f->params.restMinT = p[7];
    f->params.restFilterTau = p[8];
    f->params.restThGyr = p[9];
    f->params.restThAcc = p[10];
    f->params.biasVerticalForgettingFactor = p[11];
    f->params.motionBiasEstEnabled = p[12] != 0.0f;
    f->params.restBiasEstEnabled = p[13] != 0.0f;
    f->params.magDistRejectionEnabled = p[14] != 0.0f;
    f->params.magCurrentTau = p[15];
    f->params.magNormTh = p[16];
    f->params.magDipTh = p[17];
    f->params.magRefTau = p[18];
    f->params.magNewTime = p[19];
    f->params.magNewFirstTime = p[20];
    f->params.magNewMinGyr = p[21];
    f->params.magMinUndisturbedTime = p[22];
    f->params.magMaxRejectionTime = p[23];
    f->params.magRejectionFactor = p[24];
    initVqf(&f->params, &f->state, &f->coeffs, gyr_dt, acc_dt, mag_dt);
    return f;
}
void slime_vqf_destroy(void *ptr) { free(ptr); }
static int finite3(const float *v) { return isfinite(v[0]) && isfinite(v[1]) && isfinite(v[2]); }
void slime_vqf_reset(void *ptr) {
    replay_filter *f = ptr;
    resetState(&f->params, &f->state, &f->coeffs);
}
void slime_vqf_gyr(void *ptr, const float *g) {
    replay_filter *f = ptr;
    if (finite3(g)) updateGyr(&f->params, &f->state, &f->coeffs, g);
}
void slime_vqf_acc(void *ptr, const float *a) {
    replay_filter *f = ptr;
    if (finite3(a)) updateAcc(&f->params, &f->state, &f->coeffs, a);
}
void slime_vqf_mag(void *ptr, const float *m, float dt) {
    replay_filter *f = ptr;
    if (!finite3(m)) return;
    if (dt > 0.0f && dt < 10.0f) {
        uint64_t ts = f->state.lastMagTsUs + (uint64_t)(dt * 1e6f);
        if (ts == 0) ts = 1;
        updateMagTs(&f->params, &f->state, &f->coeffs, m, ts);
    } else updateMag(&f->params, &f->state, &f->coeffs, m);
}
/* Flat POD output avoids depending on the compiler's private struct/bool ABI. */
void slime_vqf_state(void *ptr, float *out) {
    replay_filter *f = ptr;
    /* A rejected numerical state must not reach CMSIS's float-to-index cast in
       getQuat9D. Rust treats this diagnostic NaN as a terminal processing error. */
    if (!isfinite(f->state.delta) || fabsf(f->state.delta) > 4.0f) {
        for (int i = 0; i < 22; ++i) out[i] = NAN;
        return;
    }
    getQuat6D(&f->state, out);
    getQuat9D(&f->state, out + 4);
    memcpy(out + 8, f->state.bias, 3 * sizeof(float));
    out[11] = f->state.delta;
    out[12] = f->state.restDetected;
    out[13] = f->state.magDistDetected;
    out[14] = f->state.magNormDip[0];
    out[15] = f->state.magRefNorm;
    out[16] = f->state.magNormDip[1];
    out[17] = f->state.magRefDip;
    out[18] = f->state.biasP[0];
    out[19] = f->state.biasP[4];
    out[20] = f->state.biasP[8];
    out[21] = f->state.kMagInit;
}
