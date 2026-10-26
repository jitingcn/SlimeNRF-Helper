/// Translate application UI keys, never device output, user-provided names, or arbitrary errors.
/// Stored operation results, errors, and history retain their creation language.
pub fn tr(key: &str) -> String {
    rust_i18n::t!(key).to_string()
}

#[cfg(test)]
mod tests;
