/// Compares serial-port names using the platform's existing alias rules.
/// This is name equivalence, not evidence that two ports represent the same device.
pub(crate) fn same_port_name(a: &str, b: &str) -> bool {
    if cfg!(windows) {
        a.trim_start_matches(r"\\.\")
            .eq_ignore_ascii_case(b.trim_start_matches(r"\\.\"))
    } else {
        a == b
    }
}

#[cfg(test)]
mod tests {
    use super::same_port_name;

    #[test]
    fn distinct_port_names_do_not_match() {
        assert!(same_port_name("COM8", "COM8"));
        assert!(!same_port_name("COM8", "COM80"));
    }

    #[cfg(windows)]
    #[test]
    fn windows_names_ignore_ascii_case_and_device_prefixes() {
        assert!(same_port_name("COM8", r"\\.\com8"));
        assert!(same_port_name(r"\\.\com8", "COM8"));
        assert!(same_port_name("com8", "COM8"));
        assert!(!same_port_name(r"\\?\COM8", "COM8"));
        assert!(!same_port_name("CÖM8", "cöm8"));
    }

    #[cfg(not(windows))]
    #[test]
    fn non_windows_names_require_exact_matches() {
        assert!(same_port_name("/dev/ttyACM0", "/dev/ttyACM0"));
        assert!(!same_port_name("COM8", "com8"));
        assert!(!same_port_name("COM8", r"\\.\COM8"));
        assert!(!same_port_name("/dev/ttyACM0", "/dev/ttyacm0"));
    }
}
