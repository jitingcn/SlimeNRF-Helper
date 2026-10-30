use super::{
    Workbench,
    command_panel::{CommandPreparation, PendingCommand, PrepareAction},
};
use crate::{
    features::settings::{store::Preferences, theme},
    i18n::tr,
};
use gpui_kit::{Context, Window};

impl Workbench {
    fn refresh_input_placeholders(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.settings.refresh_placeholder(window, cx);
        self.command_search.update(cx, |input, cx| {
            input.set_placeholder(tr("commands.search.placeholder"), window, cx);
        });
        self.firmware_search.update(cx, |input, cx| {
            input.set_placeholder(tr("firmware.remote.search.placeholder"), window, cx);
        });
        self.batch_input.update(cx, |input, cx| {
            input.set_placeholder(tr("serial.terminal.command_placeholder.batch"), window, cx);
        });
        for terminal in self.terminals.values() {
            terminal.input.update(cx, |input, cx| {
                input.set_placeholder(tr("serial.terminal.command_placeholder.single"), window, cx);
            });
        }
        let parameter = match &self.sidebar.pending {
            Some(PendingCommand::Parameter { preset, input, .. }) => Some((*preset, input)),
            Some(PendingCommand::Connecting(CommandPreparation {
                action:
                    PrepareAction::Preset {
                        preset,
                        input: Some(input),
                    },
                ..
            })) => Some((*preset, input)),
            _ => None,
        };
        if let Some((preset, input)) = parameter {
            input.update(cx, |input, cx| {
                input.set_placeholder(tr(preset.parameter.hint()), window, cx);
            });
        }
        self.replay
            .update(cx, |replay, cx| replay.locale_changed(window, cx));
    }

    fn refresh_pending_status(&mut self) {
        // Prepared target captions stay with their frozen command facts.
        for (port, terminal) in &mut self.terminals {
            if terminal
                .pending
                .as_ref()
                .is_some_and(|pending| pending.error.is_none())
            {
                terminal.status = Some(
                    rust_i18n::t!("serial.terminal.status.connecting", port = port)
                        .to_string()
                        .into(),
                );
            }
        }
    }

    pub(super) fn update_preferences(
        &mut self,
        preferences: Preferences,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.shutting_down || self.preferences == preferences {
            return;
        }
        let advanced_changed = self.preferences.advanced_features != preferences.advanced_features;
        if advanced_changed
            && !self
                .flash
                .prepare_advanced_features(preferences.advanced_features)
        {
            return;
        }
        let language_changed = self.preferences.language != preferences.language;
        self.preferences = preferences;
        if advanced_changed {
            self.sync_esb_page();
        }
        theme::apply(&self.preferences, cx);
        if language_changed {
            self.refresh_pending_status();
            self.refresh_input_placeholders(window, cx);
            self.refresh_temperature_locale(cx);
            self.refresh_rssi_locale(cx);
            self.refresh_notices_locale(cx);
            self.scan_inventory(cx);
        }
        self.save_preferences(cx);
    }
}
