use crate::domain::{
    DeviceIdentity, DeviceRole, Inventory, ReceiverEndpoint, TrackerLink, address_digits,
};
use serde::{Deserialize, Deserializer, Serialize, Serializer, ser::SerializeStruct};
use std::{collections::BTreeMap, fmt};

pub(crate) const MAX_RECORDS: usize = 1024;
const MAX_NICKNAME_CHARS: usize = 80;
const MAX_NICKNAME_BYTES: usize = 320;
const MAX_METADATA_BYTES: usize = 512;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct DeviceKey(HardwareIdentity);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum HardwareIdentity {
    ReceiverUsbSerial([u8; 16]),
    TrackerRfAddress([u8; 12]),
}

impl DeviceKey {
    pub(crate) fn receiver(receiver: &ReceiverEndpoint, inventory: &Inventory) -> Option<Self> {
        let key = Self::receiver_serial(receiver.serial_number.as_deref()?)?;
        let mut candidates = inventory.receivers.iter().filter(|candidate| {
            candidate
                .serial_number
                .as_deref()
                .and_then(Self::receiver_serial)
                == Some(key)
        });
        let current = candidates.next()?;
        if candidates.next().is_some()
            || current.key != receiver.key
            || current.path != receiver.path
            || inventory.receivers.iter().any(|candidate| {
                (candidate.key == receiver.key || candidate.path == receiver.path)
                    && candidate
                        .serial_number
                        .as_deref()
                        .and_then(Self::receiver_serial)
                        != Some(key)
            })
        {
            return None;
        }
        Some(key)
    }

    pub(crate) fn tracker(link: &TrackerLink) -> Option<Self> {
        if link.slot >= 16 {
            return None;
        }
        Self::tracker_address(link.address.as_deref()?)
    }

    pub(crate) fn direct_tracker(identity: &DeviceIdentity) -> Option<Self> {
        if identity.role != DeviceRole::Tracker {
            return None;
        }
        Self::tracker_address(identity.address.as_deref()?)
    }

    // These parsers also support labels for frozen sources. They prove no live route.
    pub(crate) fn receiver_serial(value: &str) -> Option<Self> {
        let bytes = value.trim().as_bytes();
        let mut digits = [0; 16];
        if bytes.len() != digits.len() {
            return None;
        }
        for (target, byte) in digits.iter_mut().zip(bytes) {
            if !byte.is_ascii_hexdigit() {
                return None;
            }
            *target = byte.to_ascii_uppercase();
        }
        Some(Self(HardwareIdentity::ReceiverUsbSerial(digits)))
    }

    pub(crate) fn tracker_address(value: &str) -> Option<Self> {
        Some(Self(HardwareIdentity::TrackerRfAddress(address_digits(
            value,
        )?)))
    }

    pub(crate) fn role(&self) -> DeviceRole {
        match self.0 {
            HardwareIdentity::ReceiverUsbSerial(_) => DeviceRole::Receiver,
            HardwareIdentity::TrackerRfAddress(_) => DeviceRole::Tracker,
        }
    }

    pub(crate) fn value(&self) -> &str {
        let digits: &[u8] = match &self.0 {
            HardwareIdentity::ReceiverUsbSerial(digits) => digits,
            HardwareIdentity::TrackerRfAddress(digits) => digits,
        };
        std::str::from_utf8(digits).expect("device key contains canonical ASCII hex")
    }

    fn namespace(&self) -> &'static str {
        match self.0 {
            HardwareIdentity::ReceiverUsbSerial(_) => "slimenrf_receiver_usb_serial_v1",
            HardwareIdentity::TrackerRfAddress(_) => "slimenrf_tracker_rf_deviceaddr_v1",
        }
    }
}

impl fmt::Display for DeviceKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}:{}", self.namespace(), self.value())
    }
}

impl Serialize for DeviceKey {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut key = serializer.serialize_struct("DeviceKey", 2)?;
        key.serialize_field("kind", self.namespace())?;
        key.serialize_field("value", self.value())?;
        key.end()
    }
}

impl<'de> Deserialize<'de> for DeviceKey {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct StoredKey {
            kind: String,
            value: String,
        }
        let stored = StoredKey::deserialize(deserializer)?;
        let key = match stored.kind.as_str() {
            "slimenrf_receiver_usb_serial_v1" => Self::receiver_serial(&stored.value),
            "slimenrf_tracker_rf_deviceaddr_v1" => Self::tracker_address(&stored.value),
            _ => None,
        }
        .filter(|key| key.value() == stored.value)
        .ok_or_else(|| serde::de::Error::custom("invalid or unsupported device identity"))?;
        Ok(key)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReportedMetadata {
    pub(crate) name: Option<String>,
    pub(crate) board: Option<String>,
    pub(crate) target: Option<String>,
    pub(crate) firmware: Option<String>,
}

impl ReportedMetadata {
    pub(crate) fn from_identity(identity: &DeviceIdentity) -> Self {
        Self {
            name: identity.name.clone(),
            board: identity.board.clone(),
            target: identity.target.clone(),
            firmware: None,
        }
    }

    fn validate(&self) -> Result<(), RegistryValidationError> {
        for value in [&self.name, &self.board, &self.target, &self.firmware]
            .into_iter()
            .flatten()
        {
            if !valid_text(value, MAX_METADATA_BYTES) {
                return Err(RegistryValidationError::InvalidMetadata);
            }
        }
        Ok(())
    }

    fn differs_from(&self, latest: &Self) -> bool {
        [
            (&self.name, &latest.name),
            (&self.board, &latest.board),
            (&self.target, &latest.target),
            (&self.firmware, &latest.firmware),
        ]
        .into_iter()
        .any(|(current, next)| next.is_some() && current != next)
    }

    fn merge_reported(&mut self, latest: &Self) -> bool {
        let mut changed = false;
        for (current, next) in [
            (&mut self.name, &latest.name),
            (&mut self.board, &latest.board),
            (&mut self.target, &latest.target),
            (&mut self.firmware, &latest.firmware),
        ] {
            if next.is_some() && current.as_ref() != next.as_ref() {
                current.clone_from(next);
                changed = true;
            }
        }
        changed
    }

    fn fill_missing(&mut self, reported: &Self) {
        for (current, next) in [
            (&mut self.name, &reported.name),
            (&mut self.board, &reported.board),
            (&mut self.target, &reported.target),
            (&mut self.firmware, &reported.firmware),
        ] {
            if current.is_none() {
                current.clone_from(next);
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DeviceObservation {
    pub(crate) key: DeviceKey,
    pub(crate) reported: ReportedMetadata,
    pub(crate) last_receiver: Option<DeviceKey>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DeviceRecord {
    pub(crate) key: DeviceKey,
    pub(crate) nickname: Option<String>,
    pub(crate) reported: ReportedMetadata,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) last_receiver: Option<DeviceKey>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct DeviceRegistry {
    records: BTreeMap<DeviceKey, DeviceRecord>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RegistryValidationError {
    TooManyRecords,
    DuplicateIdentity,
    InvalidNickname,
    InvalidMetadata,
    InvalidReceiver,
    MissingRecord,
}

impl DeviceRegistry {
    pub(crate) fn records(&self) -> impl ExactSizeIterator<Item = &DeviceRecord> {
        self.records.values()
    }

    pub(crate) fn get(&self, key: &DeviceKey) -> Option<&DeviceRecord> {
        self.records.get(key)
    }

    pub(crate) fn from_records(
        records: Vec<DeviceRecord>,
    ) -> Result<Self, RegistryValidationError> {
        if records.len() > MAX_RECORDS {
            return Err(RegistryValidationError::TooManyRecords);
        }
        let mut registry = Self::default();
        for record in records {
            record.reported.validate()?;
            validate_last_receiver(record.key, record.last_receiver)?;
            if record.nickname.as_deref().is_some_and(|nickname| {
                !valid_text(nickname, MAX_NICKNAME_BYTES)
                    || nickname.chars().count() > MAX_NICKNAME_CHARS
            }) {
                return Err(RegistryValidationError::InvalidNickname);
            }
            if registry.records.insert(record.key, record).is_some() {
                return Err(RegistryValidationError::DuplicateIdentity);
            }
        }
        Ok(registry)
    }

    pub(crate) fn observation_changes(
        &self,
        observation: &DeviceObservation,
    ) -> Result<bool, RegistryValidationError> {
        observation.reported.validate()?;
        validate_last_receiver(observation.key, observation.last_receiver)?;
        Ok(self.records.get(&observation.key).is_none_or(|record| {
            record.reported.differs_from(&observation.reported)
                || (observation.last_receiver.is_some()
                    && record.last_receiver != observation.last_receiver)
        }))
    }

    pub(crate) fn observe(
        &mut self,
        observation: &DeviceObservation,
    ) -> Result<bool, RegistryValidationError> {
        observation.reported.validate()?;
        validate_last_receiver(observation.key, observation.last_receiver)?;
        if let Some(record) = self.records.get_mut(&observation.key) {
            let mut changed = record.reported.merge_reported(&observation.reported);
            if observation.last_receiver.is_some()
                && record.last_receiver != observation.last_receiver
            {
                record.last_receiver = observation.last_receiver;
                changed = true;
            }
            return Ok(changed);
        }
        if self.records.len() == MAX_RECORDS {
            return Err(RegistryValidationError::TooManyRecords);
        }
        self.records.insert(
            observation.key,
            DeviceRecord {
                key: observation.key,
                nickname: None,
                reported: observation.reported.clone(),
                last_receiver: observation.last_receiver,
            },
        );
        Ok(true)
    }

    pub(crate) fn rename(
        &mut self,
        key: DeviceKey,
        nickname: &str,
    ) -> Result<bool, RegistryValidationError> {
        let nickname = nickname.trim();
        if !nickname.is_empty()
            && (!valid_text(nickname, MAX_NICKNAME_BYTES)
                || nickname.chars().count() > MAX_NICKNAME_CHARS)
        {
            return Err(RegistryValidationError::InvalidNickname);
        }
        let record = self
            .records
            .get_mut(&key)
            .ok_or(RegistryValidationError::MissingRecord)?;
        if record.nickname.as_deref().unwrap_or("") == nickname {
            return Ok(false);
        }
        record.nickname = (!nickname.is_empty()).then(|| nickname.to_owned());
        Ok(true)
    }

    pub(crate) fn forget(&mut self, key: DeviceKey) -> bool {
        self.records.remove(&key).is_some()
    }
}

fn validate_last_receiver(
    owner: DeviceKey,
    receiver: Option<DeviceKey>,
) -> Result<(), RegistryValidationError> {
    if let Some(receiver) = receiver
        && (owner.role() != DeviceRole::Tracker || receiver.role() != DeviceRole::Receiver)
    {
        return Err(RegistryValidationError::InvalidReceiver);
    }
    Ok(())
}

fn valid_text(value: &str, max_bytes: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_bytes
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

#[derive(Default)]
pub(crate) struct ObservedDevices {
    observations: BTreeMap<DeviceKey, ObservedEntry>,
}

struct ObservedEntry {
    observation: DeviceObservation,
    receivers: u8,
    recent_rf: u8,
    historical_rf: u8,
    direct: u8,
}

enum ObservationSource {
    Receiver,
    RecentRf(Option<DeviceKey>),
    HistoricalRf,
    Direct,
}

impl ObservedEntry {
    fn admitted(&self) -> bool {
        match self.observation.key.role() {
            DeviceRole::Receiver => self.receivers == 1,
            DeviceRole::Tracker => {
                self.recent_rf < 2
                    && self.direct < 2
                    && (self.recent_rf == 1 || self.direct == 1 || self.historical_rf == 1)
            }
            DeviceRole::Unknown => false,
        }
    }

    fn current(&self) -> bool {
        self.admitted() && (self.receivers == 1 || self.recent_rf == 1 || self.direct == 1)
    }
}

impl ObservedDevices {
    pub(crate) fn add_receiver(
        &mut self,
        receiver: &ReceiverEndpoint,
        inventory: &Inventory,
        reported: ReportedMetadata,
    ) {
        if let Some(key) = DeviceKey::receiver(receiver, inventory) {
            self.insert(key, reported, ObservationSource::Receiver);
        }
    }

    pub(crate) fn add_tracker(
        &mut self,
        link: &TrackerLink,
        receiver: Option<DeviceKey>,
        reported: ReportedMetadata,
    ) {
        if let Some(key) = DeviceKey::tracker(link) {
            let source = if link.online {
                ObservationSource::RecentRf(
                    receiver.filter(|key| key.role() == DeviceRole::Receiver),
                )
            } else {
                ObservationSource::HistoricalRf
            };
            self.insert(key, reported, source);
        }
    }

    pub(crate) fn add_direct_tracker(&mut self, identity: &DeviceIdentity) {
        if let Some(key) = DeviceKey::direct_tracker(identity) {
            self.insert(
                key,
                ReportedMetadata::from_identity(identity),
                ObservationSource::Direct,
            );
        }
    }

    fn insert(&mut self, key: DeviceKey, reported: ReportedMetadata, source: ObservationSource) {
        let entry = self
            .observations
            .entry(key)
            .or_insert_with(|| ObservedEntry {
                observation: DeviceObservation {
                    key,
                    reported: ReportedMetadata::default(),
                    last_receiver: None,
                },
                receivers: 0,
                recent_rf: 0,
                historical_rf: 0,
                direct: 0,
            });
        match source {
            ObservationSource::Receiver => {
                entry.receivers = (entry.receivers + 1).min(2);
                entry.observation.reported.merge_reported(&reported);
            }
            ObservationSource::RecentRf(receiver) => {
                entry.recent_rf = (entry.recent_rf + 1).min(2);
                entry.observation.last_receiver =
                    if entry.recent_rf == 1 { receiver } else { None };
                if entry.direct == 0 {
                    entry.observation.reported.merge_reported(&reported);
                } else {
                    entry.observation.reported.fill_missing(&reported);
                }
            }
            ObservationSource::HistoricalRf => {
                entry.historical_rf = (entry.historical_rf + 1).min(2);
            }
            ObservationSource::Direct => {
                entry.direct = (entry.direct + 1).min(2);
                entry.observation.reported.merge_reported(&reported);
            }
        }
    }

    pub(crate) fn get(&self, key: &DeviceKey) -> Option<&DeviceObservation> {
        self.observations
            .get(key)
            .filter(|entry| entry.admitted())
            .map(|entry| &entry.observation)
    }

    pub(crate) fn is_current(&self, key: &DeviceKey) -> bool {
        self.observations
            .get(key)
            .is_some_and(ObservedEntry::current)
    }

    pub(crate) fn observations(&self) -> impl Iterator<Item = &DeviceObservation> {
        self.observations
            .values()
            .filter(|entry| entry.admitted())
            .map(|entry| &entry.observation)
    }

    pub(crate) fn into_observations(self) -> Vec<DeviceObservation> {
        self.observations
            .into_values()
            .filter(ObservedEntry::admitted)
            .map(|entry| entry.observation)
            .collect()
    }
}

#[cfg(test)]
mod tests;
