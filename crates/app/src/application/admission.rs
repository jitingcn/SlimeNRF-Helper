use super::Workbench;
use crate::{
    domain::console_port_allowed, i18n::tr, serial_port::same_port_name, sessions::WriteReceipt,
};

impl Workbench {
    pub(super) fn console_check_port(&self, port: &str) -> Result<(), String> {
        if self.inventory.serial_enumeration_failed
            || self.inventory_error.is_some()
            || !self
                .port_endpoint(port)
                .is_some_and(|endpoint| console_port_allowed(endpoint, &self.inventory))
        {
            return Err(tr("serial.console.error.not_allowed"));
        }
        Ok(())
    }

    pub(super) fn flash_check_receiver(&self, key: &str) -> Result<(), String> {
        self.capture_check_receiver(key)?;
        self.calibration_check_receiver(key)?;
        self.flash_only_check_receiver(key)
    }

    pub(super) fn flash_check_receiver_io(&self, key: &str) -> Result<(), String> {
        self.capture_check_receiver(key)?;
        self.calibration_check_receiver_io(key)?;
        self.flash_only_check_receiver(key)
    }

    pub(super) fn flash_check_scoped_tracker_io(
        &self,
        receiver: &str,
        address: &str,
    ) -> Result<(), String> {
        self.capture_check_receiver(receiver)?;
        self.calibration_check_scoped_tracker_io(receiver, address)?;
        self.flash_only_check_receiver(receiver)?;
        self.flash_only_check_address(address)
    }

    pub(super) fn flash_only_check_receiver(&self, key: &str) -> Result<(), String> {
        if self.shutting_down {
            return Err(tr("firmware.status.closing"));
        }
        if self.flash.blocks_receiver(key) {
            return Err(tr("firmware.error.receiver_updating").to_string());
        }
        Ok(())
    }

    pub(super) fn flash_only_check_address(&self, address: &str) -> Result<(), String> {
        if self.shutting_down {
            return Err(tr("firmware.status.closing"));
        }
        if self.flash.blocks_address(address) {
            return Err(tr("firmware.error.receiver_updating"));
        }
        Ok(())
    }

    pub(super) fn flash_check_port(&self, port: &str) -> Result<(), String> {
        self.capture_check_port(port)?;
        self.calibration_check_port(port)?;
        self.flash_only_check_port(port)
    }

    pub(super) fn flash_only_check_port(&self, port: &str) -> Result<(), String> {
        if self.shutting_down {
            return Err(tr("firmware.status.closing"));
        }
        let endpoint = self
            .inventory
            .ports
            .iter()
            .find(|endpoint| same_port_name(&endpoint.port_name, port));
        if self.flash.blocks_port(port, endpoint) {
            return Err(tr("firmware.error.port_updating"));
        }
        Ok(())
    }

    pub(super) fn flash_send_line(
        &mut self,
        port: &str,
        command: &str,
    ) -> Result<WriteReceipt, String> {
        self.console_check_port(port)?;
        self.flash_check_port(port)?;
        self.manager.send_line(port, command)
    }
}
