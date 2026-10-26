// SPDX-FileCopyrightText: 2021 Daniel Laidig <laidig@control.tu-berlin.de>
//
// SPDX-License-Identifier: MIT

#ifndef VQF_H__
#define VQF_H__

#include <stdint.h>
#include <stdbool.h>

#define VQF_MIXED_PRECISION
// #define VQF_NO_MOTION_BIAS_ESTIMATION

#ifdef __cplusplus
extern "C" {
#endif

/**
 * @brief Typedef for the floating-point data type used for most operations.
 *
 * By default, all floating-point calculations are performed using `double`. Set the `VQF_SINGLE_PRECISION` define to
 * change ALL types (including Butterworth filter state) to `float` — note this can cause numeric issues at high
 * sample rates due to very small filter coefficients. Set `VQF_MIXED_PRECISION` to use `float` for general
 * computations but `double` for Butterworth filter coefficients and state.
 */
#ifdef VQF_SINGLE_PRECISION
typedef float vqf_real_t;
typedef float vqf_double_t;
#elif defined(VQF_MIXED_PRECISION)
typedef float vqf_real_t;
typedef double vqf_double_t;
#else   // double precision
typedef double vqf_real_t;
typedef double vqf_double_t;
#endif

/**
 * @brief Struct containing all tuning parameters used by the VQF class.
 *
 * The parameters influence the behavior of the algorithm and are independent of the sampling rate of the IMU data. The
 * constructor sets all parameters to the default values.
 *
 * The parameters #motionBiasEstEnabled, #restBiasEstEnabled, and #magDistRejectionEnabled can be used to enable/disable
 * the main features of the VQF algorithm. The time constants #tauAcc and #tauMag can be tuned to change the trust on
 * the accelerometer and magnetometer measurements, respectively. The remaining parameters influence bias estimation
 * and magnetometer rejection.
 */
typedef struct vqf_params_s
{
    /**
     * @brief Time constant \f$\tau_\mathrm{acc}\f$ for accelerometer low-pass filtering in seconds.
     *
     * Small values for \f$\tau_\mathrm{acc}\f$ imply trust on the accelerometer measurements and while large values of
     * \f$\tau_\mathrm{acc}\f$ imply trust on the gyroscope measurements.
     *
     * The time constant \f$\tau_\mathrm{acc}\f$ corresponds to the cutoff frequency \f$f_\mathrm{c}\f$ of the
     * second-order Butterworth low-pass filter as follows: \f$f_\mathrm{c} = \frac{\sqrt{2}}{2\pi\tau_\mathrm{acc}}\f$.
     *
     * Default value: 3.0 s
     */
    vqf_real_t tauAcc;
    /**
     * @brief Time constant \f$\tau_\mathrm{mag}\f$ for magnetometer update in seconds.
     *
     * Small values for \f$\tau_\mathrm{mag}\f$ imply trust on the magnetometer measurements and while large values of
     * \f$\tau_\mathrm{mag}\f$ imply trust on the gyroscope measurements.
     *
     * The time constant \f$\tau_\mathrm{mag}\f$ corresponds to the cutoff frequency \f$f_\mathrm{c}\f$ of the
     * first-order low-pass filter for the heading correction as follows:
     * \f$f_\mathrm{c} = \frac{1}{2\pi\tau_\mathrm{mag}}\f$.
     *
     * Default value: 9.0 s
     */
    vqf_real_t tauMag;

#ifndef VQF_NO_MOTION_BIAS_ESTIMATION
    /**
     * @brief Enables gyroscope bias estimation during motion phases.
     *
     * If set to true (default), gyroscope bias is estimated based on the inclination correction only, i.e. without
     * using magnetometer measurements.
     */
    bool motionBiasEstEnabled;
#endif
    /**
     * @brief Enables rest detection and gyroscope bias estimation during rest phases.
     *
     * If set to true (default), phases in which the IMU is at rest are detected. During rest, the gyroscope bias
     * is estimated from the low-pass filtered gyroscope readings.
     */
    bool restBiasEstEnabled;
    /**
     * @brief Enables magnetic disturbance detection and magnetic disturbance rejection.
     *
     * If set to true (default), the magnetic field is analyzed. For short disturbed phases, the magnetometer-based
     * correction is disabled totally. If the magnetic field is always regarded as disturbed or if the duration of
     * the disturbances exceeds #magMaxRejectionTime, magnetometer-based updates are performed, but with an increased
     * time constant.
     */
    bool magDistRejectionEnabled;

    /**
     * @brief Standard deviation of the initial bias estimation uncertainty (in degrees per second).
     *
     * Default value: 0.5 °/s
     */
    vqf_real_t biasSigmaInit;
    /**
     * @brief Time in which the bias estimation uncertainty increases from 0 °/s to 0.1 °/s (in seconds).
     *
     * This value determines the system noise assumed by the Kalman filter.
     *
     * Default value: 100.0 s
     */
    vqf_real_t biasForgettingTime;
    /**
     * @brief Maximum expected gyroscope bias (in degrees per second).
     *
     * This value is used to clip the bias estimate and the measurement error in the bias estimation update step. It is
     * further used by the rest detection algorithm in order to not regard measurements with a large but constant
     * angular rate as rest.
     *
     * Default value: 2.0 °/s
     */
    vqf_real_t biasClip;
#ifndef VQF_NO_MOTION_BIAS_ESTIMATION
    /**
     * @brief Standard deviation of the converged bias estimation uncertainty during motion (in degrees per second).
     *
     * This value determines the trust on motion bias estimation updates. A small value leads to fast convergence.
     *
     * Default value: 0.1 °/s
     */
    vqf_real_t biasSigmaMotion;
    /**
     * @brief Forgetting factor for unobservable bias in vertical direction during motion.
     *
     * As magnetometer measurements are deliberately not used during motion bias estimation, gyroscope bias is not
     * observable in vertical direction. This value is the relative weight of an artificial zero measurement that
     * ensures that the bias estimate in the unobservable direction will eventually decay to zero.
     *
     * Default value: 0.0001
     */
    vqf_real_t biasVerticalForgettingFactor;
#endif
    /**
     * @brief Standard deviation of the converged bias estimation uncertainty during rest (in degrees per second).
     *
     * This value determines the trust on rest bias estimation updates. A small value leads to fast convergence.
     *
     * Default value: 0.03 °
     */
    vqf_real_t biasSigmaRest;

    /**
     * @brief Time threshold for rest detection (in seconds).
     *
     * Rest is detected when the measurements have been close to the low-pass filtered reference for the given time.
     *
     * Default value: 1.5 s
     */
    vqf_real_t restMinT;
    /**
     * @brief Time constant for the low-pass filter used in rest detection (in seconds).
     *
     * This time constant characterizes a second-order Butterworth low-pass filter used to obtain the reference for
     * rest detection.
     *
     * Default value: 0.5 s
     */
    vqf_real_t restFilterTau;
    /**
     * @brief Angular velocity threshold for rest detection (in °/s).
     *
     * For rest to be detected, the norm of the deviation between measurement and reference must be below the given
     * threshold. (Furthermore, the absolute value of each component must be below #biasClip).
     *
     * Default value: 2.0 °/s
     */
    vqf_real_t restThGyr;
    /**
     * @brief Acceleration threshold for rest detection (in m/s²).
     *
     * For rest to be detected, the norm of the deviation between measurement and reference must be below the given
     * threshold.
     *
     * Default value: 0.5 m/s²
     */
    vqf_real_t restThAcc;

    /**
     * @brief Time constant for current norm/dip value in magnetic disturbance detection (in seconds).
     *
     * This (very fast) low-pass filter is intended to provide additional robustness when the magnetometer measurements
     * are noisy or not sampled perfectly in sync with the gyroscope measurements. Set to -1 to disable the low-pass
     * filter and directly use the magnetometer measurements.
     *
     * Default value: 0.05 s
     */
    vqf_real_t magCurrentTau;
    /**
     * @brief Time constant for the adjustment of the magnetic field reference (in seconds).
     *
     * This adjustment allows the reference estimate to converge to the observed undisturbed field.
     *
     * Default value: 20.0 s
     */
    vqf_real_t magRefTau;
    /**
     * @brief Relative threshold for the magnetic field strength for magnetic disturbance detection.
     *
     * This value is relative to the reference norm.
     *
     * Default value: 0.1 (10%)
     */
    vqf_real_t magNormTh;
    /**
     * @brief Threshold for the magnetic field dip angle for magnetic disturbance detection (in degrees).
     *
     * Default vaule: 10 °
     */
    vqf_real_t magDipTh;
    /**
     * @brief Duration after which to accept a different homogeneous magnetic field (in seconds).
     *
     * A different magnetic field reference is accepted as the new field when the measurements are within the thresholds
     * #magNormTh and #magDipTh for the given time. Additionally, only phases with sufficient movement, specified by
     * #magNewMinGyr, count.
     *
     * Default value: 20.0
     */
    vqf_real_t magNewTime;
    /**
     * @brief Duration after which to accept a homogeneous magnetic field for the first time (in seconds).
     *
     * This value is used instead of #magNewTime when there is no current estimate in order to allow for the initial
     * magnetic field reference to be obtained faster.
     *
     * Default value: 5.0
     */
    vqf_real_t magNewFirstTime;
    /**
     * @brief Minimum angular velocity needed in order to count time for new magnetic field acceptance (in °/s).
     *
     * Durations for which the angular velocity norm is below this threshold do not count towards reaching #magNewTime.
     *
     * Default value: 20.0 °/s
     */
    vqf_real_t magNewMinGyr;
    /**
     * @brief Minimum duration within thresholds after which to regard the field as undisturbed again (in seconds).
     *
     * Default value: 0.5 s
     */
    vqf_real_t magMinUndisturbedTime;
    /**
     * @brief Maximum duration of full magnetic disturbance rejection (in seconds).
     *
     * For magnetic disturbances up to this duration, heading correction is fully disabled and heading changes are
     * tracked by gyroscope only. After this duration (or for many small disturbed phases without sufficient time in the
     * undisturbed field in between), the heading correction is performed with an increased time constant (see
     * #magRejectionFactor).
     *
     * Default value: 60.0 s
     */
    vqf_real_t magMaxRejectionTime;
    /**
     * @brief Factor by which to slow the heading correction during long disturbed phases.
     *
     * After #magMaxRejectionTime of full magnetic disturbance rejection, heading correction is performed with an
     * increased time constant. This parameter (approximately) specifies the factor of the increase.
     *
     * Furthermore, after spending #magMaxRejectionTime/#magRejectionFactor seconds in an undisturbed magnetic field,
     * the time is reset and full magnetic disturbance rejection will be performed for up to #magMaxRejectionTime again.
     *
     * Default value: 2.0
     */
    vqf_real_t magRejectionFactor;
} vqf_params_t;

/**
 * @brief Struct containing the filter state of the VQF class.
 *
 * The relevant parts of the state can be accessed via functions of the VQF class, e.g. VQF::getQuat6D(),
 * VQF::getQuat9D(), VQF::getGyrBiasEstimate(), VQF::setGyrBiasEstimate(), VQF::getRestDetected() and
 * VQF::getMagDistDetected(). To reset the state to the initial values, use VQF::resetState().
 *
 * Direct access to the full state is typically not needed but can be useful in some cases, e.g. for debugging. For this
 * purpose, the state can be accessed by VQF::getState() and set by VQF::setState().
 */
typedef struct vqf_state_s {
    /**
     * @brief Angular velocity strapdown integration quaternion \f$^{\mathcal{S}_i}_{\mathcal{I}_i}\mathbf{q}\f$.
     */
    vqf_real_t gyrQuat[4];
    /**
     * @brief Inclination correction quaternion \f$^{\mathcal{I}_i}_{\mathcal{E}_i}\mathbf{q}\f$.
     */
    vqf_real_t accQuat[4];
    /**
     * @brief Heading difference \f$\delta\f$ between \f$\mathcal{E}_i\f$ and \f$\mathcal{E}\f$.
     *
     * \f$^{\mathcal{E}_i}_{\mathcal{E}}\mathbf{q} = \begin{bmatrix}\cos\frac{\delta}{2} & 0 & 0 &
     * \sin\frac{\delta}{2}\end{bmatrix}^T\f$.
     */
    vqf_real_t delta;
    /**
     * @brief True if it has been detected that the IMU is currently at rest.
     *
     * Used to switch between rest and motion gyroscope bias estimation.
     */
    bool restDetected;
    /**
     * @brief True if magnetic disturbances have been detected.
     */
    bool magDistDetected;

    /**
     * @brief Last low-pass filtered acceleration in the \f$\mathcal{I}_i\f$ frame.
     */
    vqf_real_t lastAccLp[3];
    /**
     * @brief Internal low-pass filter state for #lastAccLp.
     */
    vqf_double_t accLpState[3*2];
    /**
     * @brief Last inclination correction angular rate.
     *
     * Change to inclination correction quaternion \f$^{\mathcal{I}_i}_{\mathcal{E}_i}\mathbf{q}\f$ performed in the
     * last accelerometer update, expressed as an angular rate (in rad/s).
     */
    vqf_real_t lastAccCorrAngularRate;

    /**
     * @brief Gain used for heading correction to ensure fast initial convergence.
     *
     * This value is used as the gain for heading correction in the beginning if it is larger than the normal filter
     * gain. It is initialized to 1 and then updated to 0.5, 0.33, 0.25, ... After vqf_params_t::tauMag seconds, it is
     * set to zero.
     */
    vqf_real_t kMagInit;
    /**
     * @brief Last heading disagreement angle.
     *
     * Disagreement between the heading \f$\hat\delta\f$ estimated from the last magnetometer sample and the state
     * \f$\delta\f$ (in rad).
     */
    vqf_real_t lastMagDisAngle;
    /**
     * @brief Last heading correction angular rate.
     *
     * Change to heading \f$\delta\f$ performed in the last magnetometer update,
     * expressed as an angular rate (in rad/s).
     */
    vqf_real_t lastMagCorrAngularRate;

    /**
     * @brief Current gyroscope bias estimate (in rad/s).
     */
    vqf_real_t bias[3];
#ifndef VQF_NO_MOTION_BIAS_ESTIMATION
    /**
     * @brief Covariance matrix of the gyroscope bias estimate.
     *
     * The 3x3 matrix is stored in row-major order. Note that for numeric reasons the internal unit used is 0.01 °/s,
     * i.e. to get the standard deviation in degrees per second use \f$\sigma = \frac{\sqrt{p_{ii}}}{100}\f$.
     */
    vqf_real_t biasP[9];
#else
    // If only rest gyr bias estimation is enabled, P and K of the KF are always diagonal
    // and matrix inversion is not needed. If motion bias estimation is disabled at compile
    // time, storing the full P matrix is not necessary.
    vqf_real_t biasP;
#endif

#ifndef VQF_NO_MOTION_BIAS_ESTIMATION
    /**
     * @brief Internal state of the Butterworth low-pass filter for the rotation matrix coefficients used in motion
     * bias estimation.
     */
    vqf_double_t motionBiasEstRLpState[9*2];
    /**
     * @brief Internal low-pass filter state for the rotated bias estimate used in motion bias estimation.
     */
    vqf_double_t motionBiasEstBiasLpState[2*2];
#endif
    /**
     * @brief Last (squared) deviations from the reference of the last sample used in rest detection.
     *
     * Looking at those values can be useful to understand how rest detection is working and which thresholds are
     * suitable. The array contains the last values for gyroscope and accelerometer in the respective
     * units. Note that the values are squared.
     *
     * The method VQF::getRelativeRestDeviations() provides an easier way to obtain and interpret those values.
     */
    vqf_real_t restLastSquaredDeviations[2];
    /**
     * @brief The current duration for which all sensor readings are within the rest detection thresholds.
     *
     * Rest is detected if this value is larger or equal to vqf_params_t::restMinT.
     */
    vqf_real_t restT;
    /**
     * @brief Last low-pass filtered gyroscope measurement used as the reference for rest detection.
     *
     * Note that this value is also used for gyroscope bias estimation when rest is detected.
     */
    vqf_real_t restLastGyrLp[3];
    /**
     * @brief Internal low-pass filter state for #restLastGyrLp.
     */
    vqf_double_t restGyrLpState[3*2];
    /**
     * @brief Last low-pass filtered accelerometer measurement used as the reference for rest detection.
     */
    vqf_real_t restLastAccLp[3];
    /**
     * @brief Internal low-pass filter state for #restLastAccLp.
     */
    vqf_double_t restAccLpState[3*2];

    /**
     * @brief Last sample timestamps for each sensor in microseconds (monotonic). Zero if unknown.
     *
     * These timestamps are used by the `*Ts` update functions to compute the actual delta time between samples.
     */
    uint64_t lastGyrTsUs;
    uint64_t lastAccTsUs;
    uint64_t lastMagTsUs;

    /**
     * @brief Norm of the currently accepted magnetic field reference.
     *
     * A value of -1 indicates that no homogeneous field is found yet.
     */
    vqf_real_t magRefNorm;
    /**
     * @brief Dip angle of the currently accepted magnetic field reference.
     */
    vqf_real_t magRefDip;
    /**
     * @brief The current duration for which the current norm and dip are close to the reference.
     *
     * The magnetic field is regarded as undisturbed when this value reaches vqf_params_t::magMinUndisturbedTime.
     */
    vqf_real_t magUndisturbedT;
    /**
     * @brief The current duration for which the magnetic field was rejected.
     *
     * If the magnetic field is disturbed and this value is smaller than vqf_params_t::magMaxRejectionTime, heading
     * correction updates are fully disabled.
     */
    vqf_real_t magRejectT;
    /**
     * @brief Norm of the alternative magnetic field reference currently being evaluated.
     */
    vqf_real_t magCandidateNorm;
    /**
     * @brief Dip angle of the alternative magnetic field reference currently being evaluated.
     */
    vqf_real_t magCandidateDip;
    /**
     * @brief The current duration for which the norm and dip are close to the candidate.
     *
     * If this value exceeds vqf_params_t::magNewTime (or vqf_params_t::magNewFirstTime if #magRefNorm < 0), the current
     * candidate is accepted as the new reference.
     */
    vqf_real_t magCandidateT;
    /**
     * @brief Norm and dip angle of the current magnetometer measurements.
     *
     * Slightly low-pass filtered, see vqf_params_t::magCurrentTau.
     */
    vqf_real_t magNormDip[2];
    /**
     * @brief Internal low-pass filter state for the current norm and dip angle.
     */
    vqf_double_t magNormDipLpState[2*2];
} vqf_state_t;

/**
 * @brief Struct containing coefficients used by the VQF class.
 *
 * Coefficients are values that depend on the parameters and the sampling times, but do not change during update steps.
 * They are calculated in VQF::setup().
 */
typedef struct vqf_coeffs_s
{
    /**
     * @brief Sampling time of the gyroscope measurements (in seconds).
     */
    vqf_real_t gyrTs;
    /**
     * @brief Sampling time of the accelerometer measurements (in seconds).
     */
    vqf_real_t accTs;
    /**
     * @brief Sampling time of the magnetometer measurements (in seconds).
     */
    vqf_real_t magTs;

    /**
     * @brief Numerator coefficients of the acceleration low-pass filter.
     *
     * The array contains \f$\begin{bmatrix}b_0 & b_1 & b_2\end{bmatrix}\f$.
     */
    vqf_double_t accLpB[3];
    /**
     * @brief Denominator coefficients of the acceleration low-pass filter.
     *
     * The array contains \f$\begin{bmatrix}a_1 & a_2\end{bmatrix}\f$ and \f$a_0=1\f$.
     */
    vqf_double_t accLpA[2];

    /**
     * @brief Gain of the first-order filter used for heading correction.
     */
    vqf_real_t kMag;

    /**
     * @brief Variance of the initial gyroscope bias estimate.
     */
    vqf_real_t biasP0;
    /**
     * @brief System noise variance used in gyroscope bias estimation.
     */
    vqf_real_t biasV;
#ifndef VQF_NO_MOTION_BIAS_ESTIMATION
    /**
     * @brief Measurement noise variance for the motion gyroscope bias estimation update.
     */
    vqf_real_t biasMotionW;
    /**
     * @brief Measurement noise variance for the motion gyroscope bias estimation update in vertical direction.
     */
    vqf_real_t biasVerticalW;
#endif
    /**
     * @brief Measurement noise variance for the rest gyroscope bias estimation update.
     */
    vqf_real_t biasRestW;

    /**
     * @brief Numerator coefficients of the gyroscope measurement low-pass filter for rest detection.
     */
    vqf_double_t restGyrLpB[3];
    /**
     * @brief Denominator coefficients of the gyroscope measurement low-pass filter for rest detection.
     */
    vqf_double_t restGyrLpA[2];
    /**
     * @brief Numerator coefficients of the accelerometer measurement low-pass filter for rest detection.
     */
    vqf_double_t restAccLpB[3];
    /**
     * @brief Denominator coefficients of the accelerometer measurement low-pass filter for rest detection.
     */
    vqf_double_t restAccLpA[2];

    /**
     * @brief Gain of the first-order filter used for to update the magnetic field reference and candidate.
     */
    vqf_real_t kMagRef;
    /**
     * @brief Numerator coefficients of the low-pass filter for the current magnetic norm and dip.
     */
    vqf_double_t magNormDipLpB[3];
    /**
     * @brief Denominator coefficients of the low-pass filter for the current magnetic norm and dip.
     */
    vqf_double_t magNormDipLpA[2];
} vqf_coeffs_t;

/**
 * @brief A Versatile Quaternion-based Filter for IMU Orientation Estimation.
 *
 * \rst
 * This class implements the orientation estimation filter described in the following publication:
 *
 *
 *     D. Laidig and T. Seel. "VQF: Highly Accurate IMU Orientation Estimation with Bias Estimation and Magnetic
 *     Disturbance Rejection." Information Fusion 2023, 91, 187--204.
 *     `doi:10.1016/j.inffus.2022.10.014 <https://doi.org/10.1016/j.inffus.2022.10.014>`_.
 *     [Accepted manuscript available at `arXiv:2203.17024 <https://arxiv.org/abs/2203.17024>`_.]
 *
 * The filter can perform simultaneous 6D (magnetometer-free) and 9D (gyr+acc+mag) sensor fusion and can also be used
 * without magnetometer data. It performs rest detection, gyroscope bias estimation during rest and motion, and magnetic
 * disturbance detection and rejection. Different sampling rates for gyroscopes, accelerometers, and magnetometers are
 * supported as well. While in most cases, the defaults will be reasonable, the algorithm can be influenced via a
 * number of tuning parameters.
 *
 * To use this C++ implementation,
 *
 * 1. create a instance of the class and provide the sampling time and, optionally, parameters
 * 2. for every sample, call one of the update functions to feed the algorithm with IMU data
 * 3. access the estimation results with :meth:`getQuat6D() <VQF.getQuat6D>`, :meth:`getQuat9D() <VQF.getQuat9D>` and
 *    the other getter methods.
 *
 * If the full data is available in (row-major) data buffers, you can use :meth:`updateBatch() <VQF.updateBatch>`.
 *
 * This class is the main C++ implementation of the algorithm. Depending on use case and programming language of choice,
 * the following alternatives might be useful:
 *
 * +------------------------+--------------------------+--------------------------+---------------------------+
 * |                        | Full Version             | Basic Version            | Offline Version           |
 * |                        |                          |                          |                           |
 * +========================+==========================+==========================+===========================+
 * | **C++**                | **VQF (this class)**     | :cpp:class:`BasicVQF`    | :cpp:func:`offlineVQF`    |
 * +------------------------+--------------------------+--------------------------+---------------------------+
 * | **Python/C++ (fast)**  | :py:class:`vqf.VQF`      | :py:class:`vqf.BasicVQF` | :py:meth:`vqf.offlineVQF` |
 * +------------------------+--------------------------+--------------------------+---------------------------+
 * | **Pure Python (slow)** | :py:class:`vqf.PyVQF`    | --                       | --                        |
 * +------------------------+--------------------------+--------------------------+---------------------------+
 * | **Pure Matlab (slow)** | :mat:class:`VQF.m <VQF>` | --                       | --                        |
 * +------------------------+--------------------------+--------------------------+---------------------------+
 * \endrst
 */

void init_params(vqf_params_t *const params);
/**
 * Initializes the object with default parameters.
 *
 * In the most common case (using the default parameters and all data being sampled with the same frequency,
 * create the class like this:
 * \rst
 * .. code-block:: c++
 *
 *     VQF vqf(0.01); // 0.01 s sampling time, i.e. 100 Hz
 * \endrst
 *
 * @param gyrTs sampling time of the gyroscope measurements in seconds
 * @param accTs sampling time of the accelerometer measurements in seconds (the value of `gyrTs` is used if set to -1)
 * @param magTs sampling time of the magnetometer measurements in seconds (the value of `gyrTs` is used if set to -1)
 *
 */
void initVqf(vqf_params_t *const params, vqf_state_t *const state, vqf_coeffs_t *const coeffs, vqf_real_t gyrTs, vqf_real_t accTs, vqf_real_t magTs);
/**
 * @brief Performs gyroscope update step.
 *
 * It is only necessary to call this function directly if gyroscope, accelerometers and magnetometers have
 * different sampling rates. Otherwise, simply use #update().
 *
 * @param gyr gyroscope measurement in rad/s
 */
void updateGyr(vqf_params_t *const params, vqf_state_t *const state, vqf_coeffs_t *const coeffs, const vqf_real_t gyr[3]);
/**
 * @brief Same as #updateGyr but uses a per-sample monotonic timestamp (microseconds) to derive the sample interval.
 *
 * If the stored last timestamp is zero or the provided timestamp is invalid (non-monotonic or out-of-range),
 * the configured sampling time `coeffs->gyrTs` is used as fallback. Timestamp unit: microseconds (monotonic clock).
 *
 * @param gyr gyroscope measurement in rad/s
 * @param timestamp_us monotonic timestamp in microseconds
 */
void updateGyrTs(vqf_params_t *const params, vqf_state_t *const state, vqf_coeffs_t *const coeffs, const vqf_real_t gyr[3], uint64_t timestamp_us);
/**
 * @brief Performs accelerometer update step.
 *
 * It is only necessary to call this function directly if gyroscope, accelerometers and magnetometers have
 * different sampling rates. Otherwise, simply use #update().
 *
 * Should be called after #updateGyr and before #updateMag.
 *
 * @param acc accelerometer measurement in m/s²
 */
void updateAcc(vqf_params_t *const params, vqf_state_t *const state, vqf_coeffs_t *const coeffs, const vqf_real_t acc[3]);
/**
 * @brief Same as #updateAcc but uses a per-sample monotonic timestamp (microseconds) to derive the sample interval.
 *
 * See #updateGyrTs for behavior and fallback rules.
 *
 * @param acc accelerometer measurement in m/s²
 * @param timestamp_us monotonic timestamp in microseconds
 */
void updateAccTs(vqf_params_t *const params, vqf_state_t *const state, vqf_coeffs_t *const coeffs, const vqf_real_t acc[3], uint64_t timestamp_us);
/**
 * @brief Performs magnetometer update step.
 *
 * It is only necessary to call this function directly if gyroscope, accelerometers and magnetometers have
 * different sampling rates. Otherwise, simply use #update().
 *
 * Should be called after #updateAcc.
 *
 * @param mag magnetometer measurement in arbitrary units
 */
void updateMag(vqf_params_t *const params, vqf_state_t *const state, vqf_coeffs_t *const coeffs, const vqf_real_t mag[3]);
/**
 * @brief Same as #updateMag but uses a per-sample monotonic timestamp (microseconds) to derive the sample interval.
 *
 * See #updateGyrTs for behavior and fallback rules.
 *
 * @param mag magnetometer measurement in arbitrary units
 * @param timestamp_us monotonic timestamp in microseconds
 */
void updateMagTs(vqf_params_t *const params, vqf_state_t *const state, vqf_coeffs_t *const coeffs, const vqf_real_t mag[3], uint64_t timestamp_us);
/**
 * @brief Performs filter update step for one sample (magnetometer-free).
 * @param gyr gyroscope measurement in rad/s
 * @param acc accelerometer measurement in m/s²
 */
//void update(const vqf_real_t gyr[3], const vqf_real_t acc[3]);
/**
 * @brief Performs filter update step for one sample (with magnetometer measurement).
 * @param gyr gyroscope measurement in rad/s
 * @param acc accelerometer measurement in m/s²
 * @param mag magnetometer measurement in arbitrary units
 */
//void update(const vqf_real_t gyr[3], const vqf_real_t acc[3], const vqf_real_t mag[3]);

/**
 * @brief Performs batch update for multiple samples at once.
 *
 * In order to use this function, the input data must be available in an array in row-major order. A null pointer
 * can be passed for mag in order to skip the magnetometer update. All data must have the same sampling rate.
 *
 * The output pointer arguments must be null pointers or point to sufficiently large data buffers.
 *
 * Example usage:
 * \rst
 * .. code-block:: c++
 *
 *     int N = 1000;
 *     vqf_double_t gyr[N*3]; // fill with gyroscope measurements in rad/s
 *     vqf_double_t acc[N*3]; // fill will accelerometer measurements in m/s²
 *     vqf_double_t mag[N*3]; // fill with magnetometer measurements (arbitrary units)
 *     vqf_double_t quat9D[N*4]; // output buffer
 *
 *     VQF vqf(0.01); // 0.01 s sampling time, i.e. 100 Hz
 *     vqf.updateBatch(gyr, acc, mag, nullptr, quat9D, nullptr, nullptr, nullptr, nullptr, nullptr);
 * \endrst
 *
 * @param gyr gyroscope measurement in rad/s (N*3 elements, must not be null)
 * @param acc accelerometer measurement in m/s² (N*3 elements, must not be null)
 * @param mag magnetometer measurement in arbitrary units (N*3 elements, can be a null pointer)
 * @param N number of samples
 * @param out6D output buffer for the 6D quaternion (N*4 elements, can be a null pointer)
 * @param out9D output buffer for the 9D quaternion (N*4 elements, can be a null pointer)
 * @param outDelta output buffer for heading difference angle (N elements, can be a null pointer)
 * @param outBias output buffer for the gyroscope bias estimate (N*3 elements, can be a null pointer)
 * @param outBiasSigma output buffer for the bias estimation uncertainty (N elements, can be a null pointer)
 * @param outRest output buffer for the rest detection state (N elements, can be a null pointer)
 * @param outMagDist output buffer for the magnetic disturbance state (N elements, can be a null pointer)
 */
//void updateBatch(const vqf_real_t gyr[], const vqf_real_t acc[], const vqf_real_t mag[], size_t N,
//                 vqf_real_t out6D[], vqf_real_t out9D[], vqf_real_t outDelta[], vqf_real_t outBias[],
//                 vqf_real_t outBiasSigma[], bool outRest[], bool outMagDist[]);

/**
 * @brief Returns the angular velocity strapdown integration quaternion
 * \f$^{\mathcal{S}_i}_{\mathcal{I}_i}\mathbf{q}\f$.
 * @param out output array for the quaternion
 */
void getQuat3D(vqf_state_t *const state, vqf_real_t out[4]);
/**
 * @brief Returns the 6D (magnetometer-free) orientation quaternion
 * \f$^{\mathcal{S}_i}_{\mathcal{E}_i}\mathbf{q}\f$.
 * @param out output array for the quaternion
 */
void getQuat6D(vqf_state_t *const state, vqf_real_t out[4]);
/**
 * @brief Returns the 9D (with magnetometers) orientation quaternion
 * \f$^{\mathcal{S}_i}_{\mathcal{E}}\mathbf{q}\f$.
 * @param out output array for the quaternion
 */
void getQuat9D(vqf_state_t *const state, vqf_real_t out[4]);
/**
 * @brief Returns the heading difference \f$\delta\f$ between \f$\mathcal{E}_i\f$ and \f$\mathcal{E}\f$.
 *
 * \f$^{\mathcal{E}_i}_{\mathcal{E}}\mathbf{q} = \begin{bmatrix}\cos\frac{\delta}{2} & 0 & 0 &
 * \sin\frac{\delta}{2}\end{bmatrix}^T\f$.
 *
 * @return delta angle in rad (vqf_state_t::delta)
 */
vqf_real_t getDelta(vqf_state_t *const state);

/**
 * @brief Returns the current gyroscope bias estimate and the uncertainty.
 *
 * The returned standard deviation sigma represents the estimation uncertainty in the worst direction and is based
 * on an upper bound of the largest eigenvalue of the covariance matrix.
 *
 * @param out output array for the gyroscope bias estimate (rad/s)
 * @return standard deviation sigma of the estimation uncertainty (rad/s)
 */
vqf_real_t getBiasEstimate(vqf_state_t *const state, vqf_coeffs_t *const coeffs, vqf_real_t out[3]);
/**
 * @brief Sets the current gyroscope bias estimate and the uncertainty.
 *
 * If a value for the uncertainty sigma is given, the covariance matrix is set to a corresponding scaled identity
 * matrix.
 *
 * @param bias gyroscope bias estimate (rad/s)
 * @param sigma standard deviation of the estimation uncertainty (rad/s) - set to -1 (default) in order to not
 *        change the estimation covariance matrix
 */
void setBiasEstimate(vqf_state_t *const state, vqf_real_t bias[3], vqf_real_t sigma);
/**
 * @brief Returns true if rest was detected.
 */
bool getRestDetected(vqf_state_t *const state);
/**
 * @brief Returns true if a disturbed magnetic field was detected.
 */
bool getMagDistDetected(vqf_state_t *const state);
/**
 * @brief Returns the relative deviations used in rest detection.
 *
 * Looking at those values can be useful to understand how rest detection is working and which thresholds are
 * suitable. The output array is filled with the last values for gyroscope and accelerometer,
 * relative to the threshold. In order for rest to be detected, both values must stay below 1.
 *
 * @param out output array of size 2 for the relative rest deviations
  */
void getRelativeRestDeviations(vqf_params_t *const params, vqf_state_t *const state, vqf_real_t out[2]);
/**
 * @brief Returns the norm of the currently accepted magnetic field reference.
 */
vqf_real_t getMagRefNorm(vqf_state_t *const state);
/**
 * @brief Returns the dip angle of the currently accepted magnetic field reference.
 */
vqf_real_t getMagRefDip(vqf_state_t *const state);
/**
 * @brief Overwrites the current magnetic field reference.
 * @param norm norm of the magnetic field reference
 * @param dip dip angle of the magnetic field reference
 */
void setMagRef(vqf_state_t *const state, vqf_real_t norm, vqf_real_t dip);

/**
 * @brief Sets the time constant for accelerometer low-pass filtering.
 *
 * For more details, see vqf_params_t.tauAcc.
 *
 * @param tauAcc time constant \f$\tau_\mathrm{acc}\f$ in seconds
 */
void setTauAcc(vqf_params_t *const params, vqf_state_t *const state, vqf_coeffs_t *const coeffs, vqf_real_t tauAcc);
/**
 * @brief Sets the time constant for the magnetometer update.
 *
 * For more details, see vqf_params_t.tauMag.
 *
 * @param tauMag time constant \f$\tau_\mathrm{mag}\f$ in seconds
 */
void setTauMag(vqf_params_t *const params, vqf_coeffs_t *const coeffs, vqf_real_t tauMag);
#ifndef VQF_NO_MOTION_BIAS_ESTIMATION
/**
 * @brief Enables/disabled gyroscope bias estimation during motion.
 */
void setMotionBiasEstEnabled(vqf_params_t *const params, vqf_state_t *const state, bool enabled);
#endif
/**
 * @brief Enables/disables rest detection and bias estimation during rest.
 */
void setRestBiasEstEnabled(vqf_params_t *const params, vqf_state_t *const state, bool enabled);
/**
 * @brief Enables/disables magnetic disturbance detection and rejection.
 */
void setMagDistRejectionEnabled(vqf_params_t *const params, vqf_state_t *const state, bool enabled);
/**
 * @brief Sets the current thresholds for rest detection.
 *
 * For details about the parameters, see vqf_params_t.restThGyr and vqf_params_t.restThAcc.
 */
void setRestDetectionThresholds(vqf_params_t *const params, vqf_real_t thGyr, vqf_real_t thAcc);

/**
 * @brief Resets the state to the default values at initialization.
 *
 * Resetting the state is equivalent to creating a new instance of this class.
 */
void resetState(vqf_params_t *const params, vqf_state_t *const state, vqf_coeffs_t *const coeffs);

#ifdef __cplusplus
}
#endif

#endif // VQF_H__
