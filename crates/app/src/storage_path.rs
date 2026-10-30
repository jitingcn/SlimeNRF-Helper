use std::path::PathBuf;

pub(crate) fn preferences() -> Result<PathBuf, String> {
    directories::ProjectDirs::from("org", "SlimeNRF", "SlimeNRF Helper")
        .map(|dirs| dirs.config_dir().join("preferences.json"))
        .ok_or_else(|| crate::i18n::tr("app.settings.error.directory_unavailable"))
}
