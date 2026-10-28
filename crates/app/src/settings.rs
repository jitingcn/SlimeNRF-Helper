pub(crate) mod device_registry;
pub(crate) mod shortcuts;

use std::{
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

const MAX_STORE_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Language {
    #[default]
    #[serde(rename = "en")]
    English,
    #[serde(rename = "zh-CN")]
    SimplifiedChinese,
}

impl Language {
    pub const ALL: [Self; 2] = [Self::English, Self::SimplifiedChinese];

    pub fn code(self) -> &'static str {
        match self {
            Self::English => "en",
            Self::SimplifiedChinese => "zh-CN",
        }
    }

    pub fn label_key(self) -> &'static str {
        match self {
            Self::English => "app.settings.language.english",
            Self::SimplifiedChinese => "app.settings.language.simplified_chinese",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccentColor {
    #[default]
    Neutral,
    Blue,
    Green,
    Purple,
    Amber,
    Sakura,
}

impl AccentColor {
    pub const ALL: [Self; 6] = [
        Self::Neutral,
        Self::Blue,
        Self::Green,
        Self::Purple,
        Self::Amber,
        Self::Sakura,
    ];

    pub fn label_key(self) -> &'static str {
        match self {
            Self::Neutral => "app.settings.accent.neutral",
            Self::Blue => "app.settings.accent.blue",
            Self::Green => "app.settings.accent.green",
            Self::Purple => "app.settings.accent.purple",
            Self::Amber => "app.settings.accent.amber",
            Self::Sakura => "app.settings.accent.sakura",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Appearance {
    Light,
    #[default]
    Dark,
}

impl Appearance {
    pub const ALL: [Self; 2] = [Self::Light, Self::Dark];

    pub fn label_key(self) -> &'static str {
        match self {
            Self::Light => "app.settings.appearance.light",
            Self::Dark => "app.settings.appearance.dark",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    pub language: Language,
    pub accent: AccentColor,
    pub appearance: Appearance,
    pub advanced_features: bool,
    pub app_update_auto_check: bool,
    pub app_update_include_prereleases: bool,
    pub app_update_last_attempt: Option<u64>,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            language: Language::default(),
            accent: AccentColor::default(),
            appearance: Appearance::default(),
            advanced_features: false,
            app_update_auto_check: true,
            app_update_include_prereleases: false,
            app_update_last_attempt: None,
        }
    }
}

fn config_path() -> Result<PathBuf, String> {
    ProjectDirs::from("org", "SlimeNRF", "SlimeNRF Helper")
        .map(|dirs| dirs.config_dir().join("preferences.json"))
        .ok_or_else(|| crate::i18n::tr("app.settings.error.directory_unavailable"))
}

pub fn load() -> Result<Preferences, String> {
    load_from(&config_path()?)
}

pub fn save(preferences: &Preferences) -> Result<(), String> {
    save_to(&config_path()?, preferences)
}

#[cfg(test)]
pub(crate) fn save_to_for_test(path: &Path, preferences: &Preferences) -> Result<(), String> {
    save_to(path, preferences)
}

fn read_document(path: &Path) -> Result<Option<(Preferences, Value)>, String> {
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(rust_i18n::t!(
                "app.settings.error.read",
                path = path.display(),
                error = error
            )
            .to_string());
        }
    };
    let metadata = file
        .metadata()
        .map_err(|error| invalid_settings(path, error))?;
    if !metadata.is_file() {
        return Err(invalid_settings(
            path,
            crate::i18n::tr("app.settings.error.regular_file"),
        ));
    }
    if metadata.len() > MAX_STORE_BYTES as u64 {
        return Err(invalid_settings(
            path,
            crate::i18n::tr("app.settings.error.too_large"),
        ));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_STORE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| invalid_settings(path, error))?;
    if bytes.len() > MAX_STORE_BYTES {
        return Err(invalid_settings(
            path,
            crate::i18n::tr("app.settings.error.too_large"),
        ));
    }
    // Require an object, but retain unknown fields for forwards-compatible saves.
    let document = serde_json::from_slice::<Map<String, Value>>(&bytes)
        .map(Value::Object)
        .map_err(|error| invalid_settings(path, error))?;
    let preferences =
        Preferences::deserialize(&document).map_err(|error| invalid_settings(path, error))?;
    Ok(Some((preferences, document)))
}

fn invalid_settings(path: &Path, error: impl std::fmt::Display) -> String {
    rust_i18n::t!(
        "app.settings.error.invalid",
        path = path.display(),
        error = error
    )
    .to_string()
}

fn load_from(path: &Path) -> Result<Preferences, String> {
    load_from_with_locale(path, sys_locale::get_locale)
}

fn load_from_with_locale(
    path: &Path,
    locale: impl FnOnce() -> Option<String>,
) -> Result<Preferences, String> {
    let mut preferences = match read_document(path)? {
        Some((preferences, document)) => {
            if document.get("language").is_some() {
                return Ok(preferences);
            }
            preferences
        }
        None => Preferences::default(),
    };
    // Resolve the OS language only when no saved language exists; loading never saves.
    preferences.language = language_for_locale(locale().as_deref());
    Ok(preferences)
}

fn language_for_locale(locale: Option<&str>) -> Language {
    match locale.and_then(|locale| locale.split(['-', '_']).next()) {
        Some(primary) if primary.eq_ignore_ascii_case("zh") => Language::SimplifiedChinese,
        _ => Language::English,
    }
}

struct BoundedPreferenceWriter<'a> {
    file: &'a mut fs::File,
    remaining: usize,
}

impl Write for BoundedPreferenceWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.remaining {
            return Err(io::Error::other(crate::i18n::tr(
                "app.settings.error.too_large",
            )));
        }
        let written = self.file.write(bytes)?;
        self.remaining -= written;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

fn save_to(path: &Path, preferences: &Preferences) -> Result<(), String> {
    // A failed load is never permission to replace a user's damaged/unreadable file.
    let mut document = read_document(path)?
        .map(|(_, document)| document)
        .unwrap_or_else(|| Value::Object(Map::new()));
    let mut save = || -> Result<(), Box<dyn std::error::Error>> {
        let object = document.as_object_mut().expect("validated settings object");
        object.insert(
            "language".to_owned(),
            serde_json::to_value(preferences.language)?,
        );
        object.insert(
            "accent".to_owned(),
            serde_json::to_value(preferences.accent)?,
        );
        object.insert(
            "appearance".to_owned(),
            serde_json::to_value(preferences.appearance)?,
        );
        object.insert(
            "advanced_features".to_owned(),
            Value::Bool(preferences.advanced_features),
        );
        object.insert(
            "app_update_auto_check".to_owned(),
            Value::Bool(preferences.app_update_auto_check),
        );
        object.insert(
            "app_update_include_prereleases".to_owned(),
            Value::Bool(preferences.app_update_include_prereleases),
        );
        object.insert(
            "app_update_last_attempt".to_owned(),
            serde_json::to_value(preferences.app_update_last_attempt)?,
        );
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)?;
        // Same-directory persistence uses an atomic replacement, including on Windows.
        // No truncation of the destination occurs before successful replacement.
        let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
        let mut writer = BoundedPreferenceWriter {
            file: temporary.as_file_mut(),
            remaining: MAX_STORE_BYTES,
        };
        serde_json::to_writer_pretty(&mut writer, &document)?;
        writer.write_all(b"\n")?;
        temporary.as_file().sync_all()?;
        temporary.persist(path)?;
        Ok(())
    };
    save().map_err(|error| {
        rust_i18n::t!(
            "app.settings.error.save",
            path = path.display(),
            error = error
        )
        .to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::{
        AccentColor, Appearance, Language, MAX_STORE_BYTES, Preferences, language_for_locale,
        load_from, load_from_with_locale, save_to,
    };
    use std::fs;

    #[test]
    fn only_chinese_primary_locale_selects_simplified_chinese() {
        for locale in [
            "zh",
            "zh-CN",
            "zh-TW",
            "zh-Hant-HK",
            "ZH-hans-cn",
            "zh_CN",
            "zH_TW",
        ] {
            assert_eq!(
                language_for_locale(Some(locale)),
                Language::SimplifiedChinese,
                "{locale}"
            );
        }
        for locale in [
            None,
            Some(""),
            Some("en"),
            Some("en-US"),
            Some("ja-JP"),
            Some("unknown"),
            Some("zho"),
            Some("zhx-CN"),
            Some("en-zh"),
        ] {
            assert_eq!(language_for_locale(locale), Language::English, "{locale:?}");
        }
    }

    #[test]
    fn missing_preferences_use_system_language_without_creating_a_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("nested/preferences.json");
        for (locale, language) in [
            (Some("zh-Hant-TW"), Language::SimplifiedChinese),
            (Some("en-GB"), Language::English),
            (Some("unknown"), Language::English),
            (None, Language::English),
        ] {
            assert_eq!(
                load_from_with_locale(&path, || locale.map(str::to_owned)).unwrap(),
                Preferences {
                    language,
                    ..Preferences::default()
                }
            );
            assert!(!path.exists());
            assert!(!path.parent().unwrap().exists());
        }
    }

    #[test]
    fn update_preferences_default_and_round_trip_without_losing_other_fields() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("isolated/preferences.json");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let original = r#"{"language":"en","accent":"blue","advanced_features":true,"future":{"nested":{"list":[1,{"keep":"value"}]},"enabled":true},"other":[false,{"number":42}]}"#;
        fs::write(&path, original).unwrap();

        let mut preferences = load_from(&path).unwrap();
        assert!(preferences.app_update_auto_check);
        assert!(!preferences.app_update_include_prereleases);
        assert_eq!(preferences.app_update_last_attempt, None);
        assert_eq!(fs::read_to_string(&path).unwrap(), original);

        for (auto_check, prereleases, last_attempt) in
            [(false, true, Some(1_793_145_600)), (true, false, None)]
        {
            preferences.app_update_auto_check = auto_check;
            preferences.app_update_include_prereleases = prereleases;
            preferences.app_update_last_attempt = last_attempt;
            save_to(&path, &preferences).unwrap();
            assert_eq!(load_from(&path).unwrap(), preferences);
            let document: serde_json::Value =
                serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            let original_document: serde_json::Value = serde_json::from_str(original).unwrap();
            assert_eq!(document["future"], original_document["future"]);
            assert_eq!(document["other"], original_document["other"]);
            assert_eq!(document["accent"], "blue");
            assert_eq!(document["advanced_features"], true);
            assert_eq!(document["app_update_auto_check"], auto_check);
            assert_eq!(document["app_update_include_prereleases"], prereleases);
            assert_eq!(
                document["app_update_last_attempt"],
                serde_json::to_value(last_attempt).unwrap()
            );
        }
    }

    #[test]
    fn legacy_preferences_without_language_keep_values_and_are_not_rewritten_on_load() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("preferences.json");
        let original = r#"{"accent":"sakura","appearance":"light","advanced_features":true,"future":{"enabled":true}}"#;
        fs::write(&path, original).unwrap();
        for (locale, language) in [
            (Some("ZH_cn"), Language::SimplifiedChinese),
            (Some("de-DE"), Language::English),
            (None, Language::English),
        ] {
            assert_eq!(
                load_from_with_locale(&path, || locale.map(str::to_owned)).unwrap(),
                Preferences {
                    language,
                    accent: AccentColor::Sakura,
                    appearance: Appearance::Light,
                    advanced_features: true,
                    ..Preferences::default()
                }
            );
            assert_eq!(fs::read_to_string(&path).unwrap(), original);
        }
        let preferences = load_from_with_locale(&path, || Some("zh-CN".to_owned())).unwrap();
        save_to(&path, &preferences).unwrap();
        let document: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(document["future"]["enabled"], true);
        assert_eq!(document["language"], "zh-CN");
        assert_eq!(load_from(&path).unwrap(), preferences);
    }

    #[test]
    fn explicit_saved_language_overrides_system_language() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("preferences.json");
        for language in Language::ALL {
            let preferences = Preferences {
                language,
                ..Preferences::default()
            };
            save_to(&path, &preferences).unwrap();
            for locale in [Some("zh-CN"), Some("en-US"), None] {
                assert_eq!(
                    load_from_with_locale(&path, || locale.map(str::to_owned)).unwrap(),
                    preferences
                );
            }
        }
    }

    #[test]
    fn selected_preferences_survive_restart_and_appearance_replacement() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("nested/preferences.json");
        let selected = Preferences {
            language: Language::SimplifiedChinese,
            accent: AccentColor::Sakura,
            appearance: Appearance::Light,
            advanced_features: true,
            ..Preferences::default()
        };
        save_to(&path, &selected).unwrap();
        assert_eq!(load_from(&path).unwrap(), selected);
        let document: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(document["accent"], "sakura");
        let changed = Preferences {
            appearance: Appearance::Dark,
            ..selected
        };
        save_to(&path, &changed).unwrap();
        assert_eq!(load_from(&path).unwrap(), changed);
        let changed = Preferences {
            language: Language::English,
            ..changed
        };
        save_to(&path, &changed).unwrap();
        assert_eq!(load_from(&path).unwrap(), changed);
        let changed = Preferences {
            accent: AccentColor::Neutral,
            ..changed
        };
        save_to(&path, &changed).unwrap();
        assert_eq!(load_from(&path).unwrap(), changed);
    }

    #[test]
    fn damaged_or_unknown_option_files_are_not_overwritten() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("preferences.json");
        for original in [
            "{broken",
            "null",
            r#"{"language":"unknown","accent":"blue"}"#,
            r#"{"language":null}"#,
            r#"{"language":"en","accent":"unknown"}"#,
            r#"{"language":"en","accent":"blue","appearance":"system"}"#,
            r#"{"advanced_features":"yes"}"#,
        ] {
            fs::write(&path, original).unwrap();
            assert!(load_from(&path).is_err());
            assert!(save_to(&path, &Preferences::default()).is_err());
            assert_eq!(fs::read_to_string(&path).unwrap(), original);
        }
    }

    #[test]
    fn preferences_at_the_read_limit_load_and_preserve_unknown_fields_on_save() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("preferences.json");
        let mut original = br#"{"language":"en","future":{"keep":true}}"#.to_vec();
        original.resize(MAX_STORE_BYTES, b' ');
        fs::write(&path, &original).unwrap();

        let preferences = load_from(&path).unwrap();
        assert_eq!(preferences, Preferences::default());
        assert_eq!(fs::read(&path).unwrap(), original);
        save_to(&path, &preferences).unwrap();
        assert_eq!(load_from(&path).unwrap(), preferences);
        let document: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(document["future"]["keep"], true);
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[test]
    fn oversized_preferences_reject_load_and_save_without_replacement() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("preferences.json");
        let mut original = br#"{"language":"en","future":{"keep":true}}"#.to_vec();
        original.resize(MAX_STORE_BYTES + 1, b' ');
        fs::write(&path, &original).unwrap();

        for error in [
            load_from(&path).unwrap_err(),
            save_to(&path, &Preferences::default()).unwrap_err(),
        ] {
            assert!(error.contains(&path.display().to_string()), "{error}");
        }
        assert_eq!(fs::read(&path).unwrap(), original);
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[test]
    fn nonregular_preferences_reject_load_and_save_without_changing_contents() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("preferences.json");
        fs::create_dir(&path).unwrap();
        let child = path.join("keep.txt");
        fs::write(&child, b"not preferences").unwrap();

        assert!(load_from(&path).is_err());
        assert!(save_to(&path, &Preferences::default()).is_err());
        assert!(path.is_dir());
        assert_eq!(fs::read(&child).unwrap(), b"not preferences");
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
        assert_eq!(fs::read_dir(&path).unwrap().count(), 1);
    }

    #[test]
    fn serialized_preferences_include_newline_in_the_byte_limit() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("preferences.json");
        let empty_document = "{\n  \"accent\": \"neutral\",\n  \"advanced_features\": true,\n  \"app_update_auto_check\": true,\n  \"app_update_include_prereleases\": false,\n  \"app_update_last_attempt\": null,\n  \"appearance\": \"dark\",\n  \"future\": \"\",\n  \"language\": \"en\"\n}\n";
        let future = "x".repeat(MAX_STORE_BYTES - empty_document.len());
        let original = format!(r#"{{"future":"{future}"}}"#);
        fs::write(&path, original).unwrap();
        let preferences = Preferences {
            advanced_features: true,
            ..Preferences::default()
        };

        save_to(&path, &preferences).unwrap();
        let saved = fs::read(&path).unwrap();
        assert_eq!(saved.len(), MAX_STORE_BYTES);
        assert_eq!(saved.last(), Some(&b'\n'));
        assert_eq!(load_from(&path).unwrap(), preferences);
        let document: serde_json::Value = serde_json::from_slice(&saved).unwrap();
        assert_eq!(document["future"], future);

        let error = save_to(&path, &Preferences::default()).unwrap_err();
        assert!(error.contains(&path.display().to_string()), "{error}");
        assert_eq!(fs::read(&path).unwrap(), saved);
        assert_eq!(load_from(&path).unwrap(), preferences);
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[test]
    fn pretty_print_expansion_cannot_replace_a_loadable_preferences_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("preferences.json");
        let future = "x".repeat(MAX_STORE_BYTES - r#"{"language":"en","future":""}"#.len());
        let original = format!(r#"{{"language":"en","future":"{future}"}}"#);
        fs::write(&path, &original).unwrap();

        assert_eq!(load_from(&path).unwrap(), Preferences::default());
        let error = save_to(&path, &Preferences::default()).unwrap_err();
        assert!(error.contains(&path.display().to_string()), "{error}");
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[cfg(windows)]
    #[test]
    fn unreadable_preferences_are_not_replaced() {
        use std::os::windows::fs::OpenOptionsExt;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("preferences.json");
        let original = br#"{"language":"en","future":{"keep":true}}"#;
        fs::write(&path, original).unwrap();
        let lock = fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&path)
            .unwrap();

        assert!(load_from(&path).is_err());
        assert!(save_to(&path, &Preferences::default()).is_err());
        drop(lock);
        assert_eq!(fs::read(&path).unwrap(), original);
        assert_eq!(load_from(&path).unwrap(), Preferences::default());
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[test]
    fn legacy_preferences_default_dark_and_preserve_values_across_appearance_changes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("preferences.json");
        fs::write(
            &path,
            r#"{"language":"zh-CN","accent":"blue","future":{"enabled":true}}"#,
        )
        .unwrap();
        let mut preferences = load_from(&path).unwrap();
        assert_eq!(
            preferences,
            Preferences {
                language: Language::SimplifiedChinese,
                accent: AccentColor::Blue,
                appearance: Appearance::Dark,
                advanced_features: false,
                ..Preferences::default()
            }
        );
        for appearance in [Appearance::Light, Appearance::Dark] {
            preferences.appearance = appearance;
            save_to(&path, &preferences).unwrap();
            assert_eq!(load_from(&path).unwrap(), preferences);
            let document: serde_json::Value =
                serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            assert_eq!(document["language"], "zh-CN");
            assert_eq!(document["accent"], "blue");
            assert_eq!(
                document["appearance"],
                serde_json::to_value(appearance).unwrap()
            );
            assert_eq!(document["future"]["enabled"], true);
        }
    }

    #[test]
    fn advanced_features_toggle_persists_without_losing_other_preferences_or_unknown_keys() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("preferences.json");
        fs::write(
            &path,
            r#"{"language":"zh-CN","accent":"blue","future":{"enabled":true}}"#,
        )
        .unwrap();
        let mut preferences = load_from(&path).unwrap();
        assert!(!preferences.advanced_features);
        for enabled in [true, false, true] {
            preferences.advanced_features = enabled;
            save_to(&path, &preferences).unwrap();
            preferences = load_from(&path).unwrap();
            assert_eq!(preferences.advanced_features, enabled);
            assert_eq!(preferences.language, Language::SimplifiedChinese);
            assert_eq!(preferences.accent, AccentColor::Blue);
            for appearance in [Appearance::Light, Appearance::Dark] {
                let changed = Preferences {
                    language: Language::English,
                    accent: AccentColor::Purple,
                    appearance,
                    ..preferences.clone()
                };
                save_to(&path, &changed).unwrap();
                assert_eq!(load_from(&path).unwrap(), changed);
                let document: serde_json::Value =
                    serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
                assert_eq!(document["advanced_features"], enabled);
                assert_eq!(document["future"]["enabled"], true);
            }
        }
    }

    #[cfg(windows)]
    #[test]
    fn denied_atomic_replacement_keeps_the_previous_file() {
        use std::os::windows::fs::OpenOptionsExt;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("preferences.json");
        let original = Preferences {
            language: Language::SimplifiedChinese,
            accent: AccentColor::Blue,
            appearance: Appearance::Light,
            advanced_features: true,
            ..Preferences::default()
        };
        save_to(&path, &original).unwrap();
        let bytes = fs::read(&path).unwrap();
        // Allow reads/writes, but deny delete-sharing required for replacement.
        let lock = fs::OpenOptions::new()
            .read(true)
            .share_mode(0x1 | 0x2)
            .open(&path)
            .unwrap();
        let changed = Preferences {
            accent: AccentColor::Green,
            appearance: Appearance::Dark,
            ..original.clone()
        };
        assert!(save_to(&path, &changed).is_err());
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert_eq!(load_from(&path).unwrap(), original);
        drop(lock);
        save_to(&path, &changed).unwrap();
        assert_eq!(load_from(&path).unwrap(), changed);
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    }
}
