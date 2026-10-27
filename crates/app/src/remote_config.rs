use serde::Deserialize;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GithubRepository {
    pub owner: String,
    pub name: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct ApplicationSources {
    pub releases: GithubRepository,
    pub tracker_ci: GithubRepository,
    pub receiver_ci: GithubRepository,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct BootloaderSources {
    pub releases: GithubRepository,
    pub ci: GithubRepository,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct RemoteConfig {
    pub application: ApplicationSources,
    pub bootloader: BootloaderSources,
}

fn repository(name: &str) -> GithubRepository {
    GithubRepository {
        owner: "jitingcn".into(),
        name: name.into(),
    }
}

impl Default for ApplicationSources {
    fn default() -> Self {
        Self {
            releases: repository("SlimeVR-Tracker-nRF"),
            tracker_ci: repository("SlimeVR-Tracker-nRF"),
            receiver_ci: repository("SlimeVR-Tracker-nRF-Receiver"),
        }
    }
}

impl Default for BootloaderSources {
    fn default() -> Self {
        Self {
            releases: repository("Adafruit_nRF52_Bootloader"),
            ci: repository("Adafruit_nRF52_Bootloader"),
        }
    }
}

impl RemoteConfig {
    pub fn validate(&self) -> Result<(), String> {
        for (label, repository) in [
            ("application.releases", &self.application.releases),
            ("application.tracker_ci", &self.application.tracker_ci),
            ("application.receiver_ci", &self.application.receiver_ci),
            ("bootloader.releases", &self.bootloader.releases),
            ("bootloader.ci", &self.bootloader.ci),
        ] {
            for (field, segment) in [("owner", &repository.owner), ("name", &repository.name)] {
                if segment.is_empty()
                    || matches!(segment.as_str(), "." | "..")
                    || !segment.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')
                    })
                {
                    return Err(format!(
                        "remote_firmware.{label}.{field} must be a nonempty single GitHub path segment"
                    ));
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::RemoteConfig;

    #[test]
    fn defaults_keep_application_and_bootloader_releases_separate() {
        let sources = RemoteConfig::default();
        sources.validate().unwrap();
        assert_eq!(sources.application.releases.name, "SlimeVR-Tracker-nRF");
        assert_eq!(
            sources.bootloader.releases.name,
            "Adafruit_nRF52_Bootloader"
        );
    }

    #[test]
    fn repository_segments_cannot_escape_or_inject_urls() {
        for segment in [
            "", ".", "..", "a/b", "a\\b", "a?b", "a#b", "a b", "a%2fb", "a\n",
        ] {
            let mut sources = RemoteConfig::default();
            sources.application.releases.owner = segment.into();
            assert!(sources.validate().is_err(), "{segment:?}");
            sources = RemoteConfig::default();
            sources.bootloader.releases.name = segment.into();
            assert!(sources.validate().is_err(), "{segment:?}");
            sources = RemoteConfig::default();
            sources.bootloader.ci.owner = segment.into();
            assert!(sources.validate().is_err(), "{segment:?}");
            sources = RemoteConfig::default();
            sources.bootloader.ci.name = segment.into();
            assert!(sources.validate().is_err(), "{segment:?}");
        }
    }
}
