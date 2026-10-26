use crate::receiver_events::{EventWatchStatus, Watch};

#[derive(Clone)]
pub(super) struct Settings {
    pub enabled: bool,
    pub generation: u64,
    pub status: EventWatchStatus,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: false,
            generation: 0,
            status: EventWatchStatus::Disabled,
        }
    }
}

impl Settings {
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
        self.generation = self.generation.wrapping_add(1);
        self.status = if enabled {
            EventWatchStatus::Checking
        } else {
            EventWatchStatus::Disabled
        };
    }

    pub fn for_reader_restart(&self) -> Self {
        let mut settings = self.clone();
        if settings.enabled
            && !matches!(
                settings.status,
                EventWatchStatus::Unsupported(_) | EventWatchStatus::Error(_)
            )
        {
            settings.status = EventWatchStatus::Checking;
        }
        settings
    }

    pub fn watch(&self) -> Watch {
        Watch::new(
            self.enabled,
            self.generation,
            self.for_reader_restart().status,
            255,
            31,
        )
    }
}
