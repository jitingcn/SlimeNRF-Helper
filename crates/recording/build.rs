fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=vendor/vqf");
    cc::Build::new()
        .files(["vendor/vqf/src/vqf.c", "vendor/vqf/replay_bridge.c"])
        .files([
            "vendor/vqf/cmsis/src/arm_sin_cos_f32.c",
            "vendor/vqf/cmsis/src/arm_atan2_f32.c",
            "vendor/vqf/cmsis/src/arm_dot_prod_f32.c",
            "vendor/vqf/cmsis/src/arm_scale_f32.c",
            "vendor/vqf/cmsis/src/arm_clip_f32.c",
            "vendor/vqf/cmsis/src/arm_fill_f32.c",
            "vendor/vqf/cmsis/src/sin_table.c",
        ])
        .include("vendor/vqf/cmsis/include")
        .define("CONFIG_CMSIS_DSP", "1")
        .include("vendor/vqf/src")
        .flag_if_supported("-std=c17")
        .flag_if_supported("/std:c17")
        .flag_if_supported("-ffp-contract=off")
        .flag_if_supported("/fp:precise")
        .warnings(false)
        .compile("firmware_vqf");
}
