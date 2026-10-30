use super::Workbench;
use super::device_coordinator::{
    InventoryScanIntent, InventoryScanTrigger, inventory_scan_trigger,
};
use gpui_kit::{Context, Window};
use std::time::{Duration, Instant};

impl Workbench {
    pub(super) fn advance_serial_sessions(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.shutting_down {
            return false;
        }
        let polled = self.poll_sessions();
        if polled.state_changed {
            self.sync_registry_observations(cx);
        }
        polled.redraw | self.advance_window_state(window, cx)
    }

    pub(super) fn advance_background_work(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.shutting_down {
            return false;
        }
        let changed = self.monitor.poll()
            | self.poll_receiver_binding_changes()
            | self.poll_writes()
            | self.poll_custom_commands()
            | self.poll_serial_dfu(cx)
            | self.poll_flash()
            | self.poll_capture(cx)
            | self.poll_calibration(cx);
        self.replay.update(cx, |replay, cx| replay.poll(window, cx));
        let changed = changed
            | self.poll_hid_log()
            | self.poll_temperature(cx)
            | self.poll_rssi_scans(cx)
            | self.poll_app_update(cx);
        if changed {
            self.sync_registry_observations(cx);
        }
        let now = Instant::now();
        if let Some(trigger) =
            inventory_scan_trigger(self.scanning_inventory, self.next_enumeration, now, || {
                self.serial_notifications.take_dirty()
            })
        {
            if trigger == InventoryScanTrigger::NativeChange {
                self.manager.note_native_change(Instant::now());
            }
            self.next_enumeration = now + Duration::from_secs(3);
            self.start_inventory_scan(cx, InventoryScanIntent::Scheduled);
        }
        changed | self.advance_window_state(window, cx)
    }

    pub(super) fn advance_window_state(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.shutting_down {
            return false;
        }
        let mut changed = self.sync_device_tracker_availability();
        changed |= self.sync_tabs(window, cx);
        changed |= self.sync_nickname_editor();
        changed |= self.complete_custom_terminal_clear(window, cx);
        changed |= self.resume_preparation();
        changed |= self.resume_terminal_command(window, cx);
        changed
    }
}

#[cfg(test)]
mod tests;
