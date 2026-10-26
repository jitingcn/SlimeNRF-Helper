/* SPDX-License-Identifier: Apache-2.0
 * Minimal host declarations for the unmodified CMSIS-DSP scalar source subset.
 * Types/constants mirror CMSIS-DSP (Copyright ARM Limited or its affiliates).
 * No numerical algorithm is substituted here; sqrt is the extracted upstream body.
 */
#ifndef SLIME_CMSIS_SCALAR_H
#define SLIME_CMSIS_SCALAR_H
#include <stdint.h>
#include <math.h>
typedef float float32_t;
typedef enum {
    ARM_MATH_SUCCESS = 0, ARM_MATH_ARGUMENT_ERROR = -1,
    ARM_MATH_LENGTH_ERROR = -2, ARM_MATH_SIZE_MISMATCH = -3,
    ARM_MATH_NANINF = -4, ARM_MATH_SINGULAR = -5,
    ARM_MATH_TEST_FAILURE = -6, ARM_MATH_DECOMPOSITION_FAILURE = -7
} arm_status;
#define ARM_DSP_ATTRIBUTE
#define ARM_DSP_TABLE_ATTRIBUTE
#define __STATIC_FORCEINLINE static inline
#define FAST_MATH_TABLE_SIZE 512
#define PI 3.14159265358979f
/* Select CMSIS's supported scalar host sqrt branch, not ARM inline assembly. */
#define __GNUC_PYTHON__ 1
#include "sqrt_f32.h"
extern const float32_t sinTable_f32[FAST_MATH_TABLE_SIZE + 1];
void arm_sin_cos_f32(float32_t theta, float32_t *s, float32_t *c);
arm_status arm_atan2_f32(float32_t y, float32_t x, float32_t *result);
void arm_dot_prod_f32(const float32_t *a, const float32_t *b, uint32_t n, float32_t *result);
void arm_scale_f32(const float32_t *src, float32_t scale, float32_t *dst, uint32_t n);
void arm_clip_f32(const float32_t *src, float32_t *dst, float32_t low, float32_t high, uint32_t n);
void arm_fill_f32(float32_t value, float32_t *dst, uint32_t n);
#endif
