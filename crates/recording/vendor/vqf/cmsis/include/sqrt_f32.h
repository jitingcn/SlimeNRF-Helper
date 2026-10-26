/* Copyright (C) ARM Limited or its affiliates. SPDX-License-Identifier: Apache-2.0
 * Exact arm_sqrt_f32 definition extracted from CMSIS-DSP fast_math_functions.h. */
__STATIC_FORCEINLINE arm_status arm_sqrt_f32(
  const float32_t in,
  float32_t * pOut)
  {
    if (in >= 0.0f)
    {
#if defined ( __CC_ARM )
  #if defined __TARGET_FPU_VFP
      *pOut = __sqrtf(in);
  #else
      *pOut = sqrtf(in);
  #endif

#elif defined ( __ICCARM__ )
  #if defined __ARMVFP__
      __ASM("VSQRT.F32 %0,%1" : "=t"(*pOut) : "t"(in));
  #else
      *pOut = sqrtf(in);
  #endif

#elif defined ( __ARMCC_VERSION ) && ( __ARMCC_VERSION >= 6010050 )
      *pOut = _sqrtf(in);
#elif defined(__GNUC_PYTHON__)
      *pOut = sqrtf(in);
#elif defined ( __GNUC__ )
  #if defined (__VFP_FP__) && !defined(__SOFTFP__)
      __ASM("VSQRT.F32 %0,%1" : "=t"(*pOut) : "t"(in));
  #else
      *pOut = sqrtf(in);
  #endif
#else
      *pOut = sqrtf(in);
#endif

      return (ARM_MATH_SUCCESS);
    }
    else
    {
      *pOut = 0.0f;
      return (ARM_MATH_ARGUMENT_ERROR);
    }
  }
