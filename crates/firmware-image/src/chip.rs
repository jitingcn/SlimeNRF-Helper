use super::{Error, ErrorKind, Uf2Record, u32le, uf2_records};

/// nRF52 chip layouts supported for bootloader updater validation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Nrf52Chip {
    /// nRF52833 with 512 KiB flash.
    Nrf52833,
    /// nRF52840 with 1 MiB flash.
    Nrf52840,
}

impl Nrf52Chip {
    pub(crate) fn application_family(self) -> u32 {
        match self {
            Self::Nrf52833 => 0x621e_937a,
            Self::Nrf52840 => 0xada5_2840,
        }
    }

    fn flash_bytes(self) -> u32 {
        match self {
            Self::Nrf52833 => 512 * 1024,
            Self::Nrf52840 => 1024 * 1024,
        }
    }

    pub(crate) fn validate_bootloader(self, bytes: &[u8]) -> Result<(), Error> {
        let (records, metadata) = uf2_records(bytes)?;
        if metadata.family_id != Some(0xd663_823c) {
            return Err(Error::new(
                ErrorKind::Unsupported,
                "Bootloader updates require UF2 family 0xd663823c, not an application image",
            ));
        }
        let flash = self.flash_bytes();
        // Adafruit_nRF52_Bootloader b43e929, linker/nrf52833.ld and
        // linker/nrf52840.ld: a 40 KiB bootloader reservation, 2 KiB CF2
        // region, then the MBR parameter and settings pages. These are the
        // supported updater layouts, not offsets inferred from a filename.
        let boot_start = flash - 0xc000;
        let params = flash - 0x2000;
        let config = params - 0x800;
        let mut has_code = false;
        let mut has_uicr = false;
        for &(address, data) in &records {
            // ghostfat.c is_uf2_block requires 256-byte aligned payloads.
            // uf2_records rejects NOFLASH and unsupported flags rather than
            // letting comment records supply authorization metadata.
            if data.len() != 256 || !address.is_multiple_of(256) {
                return Err(Error::malformed(
                    "Bootloader UF2 requires aligned 256-byte payloads",
                ));
            }
            let end = address + data.len() as u32;
            if address == 0x1000_1000 {
                if u32le(data, 0x14) != boot_start || u32le(data, 0x18) != params {
                    return Err(Error::new(
                        ErrorKind::Unsupported,
                        format!("Bootloader UICR layout does not match {self:?}"),
                    ));
                }
                has_uicr = true;
            } else if end <= 0x1000 {
                // nosd updaters carry the MBR, which the device skips.
            } else if address >= boot_start && end <= params {
                has_code |= address < config;
            } else {
                return Err(Error::new(
                    ErrorKind::Unsupported,
                    format!(
                        "Bootloader UF2 address {address:#010x}..{end:#010x} is outside the {self:?} bootloader/MBR layout"
                    ),
                ));
            }
        }
        if !has_uicr || !has_code {
            return Err(Error::malformed(
                "Bootloader UF2 is missing its UICR or bootloader code",
            ));
        }
        self.validate_cf2(&records, config, params)
    }

    fn validate_cf2(self, records: &[Uf2Record<'_>], config: u32, end: u32) -> Result<(), Error> {
        // pinconfig.c / uf2/configkeys.h: two magic words, used and total
        // entry counts, then little-endian (key, value) pairs. Only used
        // entries must be present in the UF2; total describes the reserved
        // capacity and is deliberately larger than the emitted initializer.
        if word_at(records, config)? != 0x1e9e_10f1 || word_at(records, config + 4)? != 0x2022_7a79
        {
            return Err(Error::malformed(
                "Bootloader UF2 is missing the supported CF2 configuration header",
            ));
        }
        let used = word_at(records, config + 8)?;
        let total = word_at(records, config + 12)?;
        if used == 0 || used > total || total > (end - config - 16) / 8 {
            return Err(Error::malformed(
                "Bootloader UF2 has invalid CF2 entry counts",
            ));
        }
        let mut flash = None;
        let mut ram = None;
        let mut family = None;
        for entry in 0..used {
            let address = config + 16 + entry * 8;
            let key = word_at(records, address)?;
            let value = word_at(records, address + 4)?;
            let field = match key {
                204 => &mut flash,
                205 => &mut ram,
                209 => &mut family,
                _ => continue,
            };
            if field.replace(value).is_some() {
                return Err(Error::malformed(format!(
                    "Bootloader UF2 has duplicate CF2 chip key {key}"
                )));
            }
        }
        let expected_ram = match self {
            Self::Nrf52833 => 128 * 1024,
            Self::Nrf52840 => 256 * 1024,
        };
        if flash != Some(self.flash_bytes())
            || ram != Some(expected_ram)
            || family != Some(self.application_family())
        {
            return Err(Error::new(
                ErrorKind::Unsupported,
                format!("Bootloader UF2 CF2 chip identity does not match {self:?}"),
            ));
        }
        Ok(())
    }
}

fn word_at(records: &[Uf2Record<'_>], address: u32) -> Result<u32, Error> {
    let index = records.partition_point(|(start, _)| *start <= address);
    if let Some(&(start, data)) = index.checked_sub(1).and_then(|index| records.get(index)) {
        let offset = (address - start) as usize;
        if offset + 4 <= data.len() {
            return Ok(u32le(data, offset));
        }
    }
    Err(Error::malformed(format!(
        "Bootloader UF2 is missing CF2 data at {address:#010x}"
    )))
}
