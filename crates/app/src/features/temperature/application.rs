use crate::application::Workbench;
use crate::{
    domain::DeviceRole,
    features::{
        commands::policy::{self as commands, CommandContext, CommandSnapshot, RouteEvidence},
        temperature::window::{DumpRequest, TemperatureRequest, TrackerTarget},
    },
    i18n::tr,
    sessions::{ConnectionState, ThermalDumpOutcome, ThermalDumpRequest},
};
use futures_util::future::Shared;
use gpui_kit::{Context, Task};

impl Workbench {
    fn temperature_content(&mut self, cx: &mut Context<Self>) {
        if self.temperature.is_initialized() {
            return;
        }
        let parent = cx.weak_entity();
        self.temperature.initialize(
            move |request, cx| {
                let _ = parent.update(cx, |this, cx| {
                    if this.shutting_down {
                        return;
                    }
                    match request {
                        TemperatureRequest::Acquire { port, generation } => {
                            this.acquire_temperature_dump(&port, generation, cx);
                        }
                        TemperatureRequest::Cancel {
                            port,
                            generation,
                            request_id,
                        } => {
                            this.manager.cancel_thermal_dump(&ThermalDumpRequest {
                                port,
                                generation,
                                request_id,
                            });
                            this.poll_temperature(cx);
                        }
                    }
                });
            },
            cx,
        );
    }

    pub(crate) fn open_temperature_window(&mut self, cx: &mut Context<Self>) {
        if self.shutting_down {
            return;
        }
        self.temperature_content(cx);
        match self.temperature.open(cx) {
            Ok(()) => self.publish_temperature_devices(cx),
            Err(error) => {
                self.error = Some(error.into());
                cx.notify();
            }
        }
    }

    pub(crate) fn refresh_temperature_locale(&self, cx: &mut Context<Self>) {
        self.temperature.refresh_locale(cx);
    }

    pub(crate) fn shutdown_temperature(&mut self, cx: &mut Context<Self>) -> Vec<Shared<Task<()>>> {
        self.temperature.shutdown(cx)
    }

    pub(crate) fn publish_temperature_devices(&self, cx: &mut Context<Self>) {
        if !self.temperature.is_open(cx) {
            return;
        }
        let active = self
            .manager
            .active_thermal_dumps()
            .into_iter()
            .map(|request| DumpRequest::new(request.port, request.generation, request.request_id))
            .collect();
        let trackers = self
            .manager
            .sessions()
            .filter(|session| {
                session.state == ConnectionState::Connected
                    && session.identity.role == DeviceRole::Tracker
            })
            .map(|session| {
                TrackerTarget::new(
                    session.port.clone(),
                    session.generation,
                    self.serial_generation_label(
                        &session.port,
                        session.generation,
                        session.port.clone(),
                    ),
                )
            })
            .collect();
        self.temperature.publish_devices(active, trackers, cx);
    }

    pub(crate) fn poll_temperature(&mut self, cx: &mut Context<Self>) -> bool {
        if self.shutting_down {
            return false;
        }
        for event in self.manager.drain_thermal_dump_events() {
            let source = self.serial_generation_label(
                &event.request.port,
                event.request.generation,
                format!(
                    "{} · #{} · {}",
                    event.request.port, event.request.generation, event.request.request_id
                ),
            );
            self.temperature_content(cx);
            match event.outcome {
                ThermalDumpOutcome::Complete(dump) => {
                    self.temperature.analyze(dump, source, cx);
                    self.open_temperature_window(cx);
                }
                outcome => {
                    let message = match outcome {
                        ThermalDumpOutcome::Failed(error) => error.to_string(),
                        ThermalDumpOutcome::WriteFailed(error) => {
                            format!("{}: {error}", tr("temperature.dump.error.write"))
                        }
                        ThermalDumpOutcome::Cancelled => tr("temperature.dump.status.cancelled"),
                        ThermalDumpOutcome::Disconnected => {
                            tr("temperature.dump.error.disconnected")
                        }
                        ThermalDumpOutcome::Handoff => tr("temperature.dump.error.handoff"),
                        ThermalDumpOutcome::TimedOut => tr("temperature.dump.error.timeout"),
                        ThermalDumpOutcome::GenerationReplaced => {
                            tr("temperature.dump.error.connection_changed")
                        }
                        ThermalDumpOutcome::Complete(_) => unreachable!(),
                    };
                    let reconnect = if self
                        .manager
                        .thermal_dump_needs_reconnect(&event.request.port)
                    {
                        format!(" {}", tr("temperature.dump.hint.reconnect"))
                    } else {
                        String::new()
                    };
                    self.temperature
                        .set_error(format!("{source}: {message}{reconnect}"), cx);
                    self.open_temperature_window(cx);
                }
            }
        }
        self.publish_temperature_devices(cx);
        // Child redraws are independent of the main page and terminal.
        false
    }

    fn acquire_temperature_dump(&mut self, port: &str, generation: u64, cx: &mut Context<Self>) {
        if self.shutting_down {
            return;
        }
        self.poll_sessions();
        let result = (|| {
            let session = self
                .manager
                .session(port)
                .ok_or_else(|| tr("temperature.dump.error.connect_tracker"))?;
            if session.generation != generation
                || session.state != ConnectionState::Connected
                || session.identity.role != DeviceRole::Tracker
            {
                return Err(tr("temperature.dump.error.target_changed"));
            }
            let preset = commands::presets(CommandContext::Direct(DeviceRole::Tracker))
                .find(|preset| preset.command == "tcal dump")
                .ok_or_else(|| tr("temperature.dump.error.command_unavailable"))?;
            let snapshot = CommandSnapshot::capture(
                preset,
                "",
                RouteEvidence::Direct {
                    port,
                    generation,
                    connected: true,
                    role: session.identity.role,
                },
            )?;
            self.flash_send_line(port, snapshot.command()).map(|_| ())
        })();
        self.temperature_content(cx);
        self.temperature.record_acquisition(port, result, cx);
        self.publish_temperature_devices(cx);
    }
}

#[cfg(test)]
mod tests {
    use crate::test_support::UiTestSession;
    use gpui_kit::{AppContext as _, TestAppContext, WindowOptions};

    #[gpui_kit::test]
    fn application_temperature_entry_retains_one_owner_and_transfers_cleanup(
        cx: &mut TestAppContext,
    ) {
        let _ui_session = UiTestSession::new(cx);
        let runtime = crate::app_runtime::AppRuntime::new().expect("create isolated I/O runtime");
        let executor = runtime.executor();
        let (_, workbench) = cx.update(|cx| {
            gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
                cx.new(|cx| {
                    crate::application::test_support::offline_workbench(
                        window,
                        cx,
                        crate::sessions::SessionManager::new(),
                        executor,
                    )
                })
            })
            .expect("open offline application")
        });
        cx.update(|cx| {
            workbench.update(cx, |this, cx| {
                assert!(!this.temperature.is_initialized());
                this.open_temperature_window(cx);
                assert!(this.temperature.is_open(cx));
                let windows = cx.windows().len();
                this.open_temperature_window(cx);
                assert_eq!(cx.windows().len(), windows);
                assert!(this.shutdown_temperature(cx).is_empty());
                assert!(!this.temperature.is_initialized());
            });
        });
    }
}
