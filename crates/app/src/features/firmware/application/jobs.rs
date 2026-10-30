use super::*;

impl Image {
    fn into_file(self) -> firmware_update::FileImage {
        firmware_update::FileImage {
            name: self.name,
            bytes: self.bytes,
        }
    }
}

impl Action {
    pub(super) fn into_request(self) -> firmware_update::ExecutionRequest {
        use firmware_update::{ExecutionRequest, UpdateRequest};
        let request = match self {
            Self::Uf2(volumes, image, purpose) => {
                UpdateRequest::Uf2(volumes, image.into_file(), purpose)
            }
            Self::Reboot(volumes) => UpdateRequest::Reboot(volumes),
            Self::Enter(port) => UpdateRequest::Enter(port),
            Self::OpenDfu(port, image, options) => {
                UpdateRequest::OpenDfu(port, image.into_file(), options)
            }
            Self::QueryGroups(plan) => UpdateRequest::QueryGroups(plan.groups),
            Self::EsbGroups(groups) => UpdateRequest::EsbGroups(
                groups
                    .into_iter()
                    .map(|group| firmware_update::EsbGroup {
                        targets: group.targets,
                        image: group.image.parsed,
                    })
                    .collect(),
            ),
            Self::Ble(devices, image) => {
                return ExecutionRequest::Ble(devices, image.into_file());
            }
        };
        ExecutionRequest::Blocking(request)
    }
}

impl FlashState {
    pub(in crate::application) fn blocks_address(&self, address: &str) -> bool {
        self.operation_pins.addresses.contains(address)
    }

    pub(super) fn release_leases(
        &mut self,
        manager: &mut crate::sessions::SessionManager,
        monitor: &mut crate::discovery::ReceiverMonitor,
    ) {
        if self.job.is_some() {
            return;
        }
        self.operation_pins = OperationPins::default();
        let mut release_error = None;
        for lease in self.leases.drain(..) {
            let result = match lease {
                Lease::Serial { token, .. } => manager.release(&token),
                Lease::Hid(token) => monitor.release(&token),
            };
            if let Err(error) = result {
                release_error = Some(error);
            }
        }
        if let Some(error) = release_error {
            self.set_activity_error(error);
        }
    }

    pub(super) fn poll_job(&mut self) -> Option<(Result<Payload, String>, Option<Progress>)> {
        let job = self.job.as_mut()?;
        let result = job.poll()?;
        let progress = job.progress.take();
        self.job = None;
        Some((result, progress))
    }

    pub(in crate::application) fn stop_for_shutdown(
        &mut self,
    ) -> ([Option<Job<Payload>>; 2], Option<Job<Image>>) {
        self.auto = false;
        self.auto_requested = false;
        self.pending = None;
        self.review = None;
        self.chooser = None;
        let jobs = [self.job.take(), self.volume_scan.take()];
        for job in jobs.iter().flatten() {
            job.cancel();
        }
        let default_job = self.default_job.take().map(|(_, job)| job);
        if let Some(job) = &default_job {
            job.cancel();
        }
        (jobs, default_job)
    }
}

impl Workbench {
    pub(in crate::application) fn ensure_flash_catalog(&mut self) {
        if self.flash.catalog_loaded || self.flash.busy() {
            return;
        }
        self.start_flash_job(|_, _| {
            let catalog = board_catalog::Catalog::load_default();
            let info = catalog
                .as_ref()
                .map(|catalog| catalog.info_filename.as_str())
                .unwrap_or("INFO_UF2.TXT");
            let volumes = uf2::scan_with_info(info)?;
            Ok(Payload::Catalog(catalog, volumes))
        });
        self.flash.ble_activity = false;
    }

    pub(super) fn start_flash_job(
        &mut self,
        work: impl FnOnce(&AtomicBool, &mut dyn FnMut(Progress)) -> Result<Payload, String>
        + Send
        + 'static,
    ) {
        if self.shutting_down || self.flash.busy() {
            return;
        }
        self.start_flash_job_for(self.flash_mode(), ActivityKind::Preparation, work);
    }

    pub(super) fn start_flash_job_for(
        &mut self,
        mode: Mode,
        kind: ActivityKind,
        work: impl FnOnce(&AtomicBool, &mut dyn FnMut(Progress)) -> Result<Payload, String>
        + Send
        + 'static,
    ) {
        if self.shutting_down || self.flash.busy() {
            return;
        }
        self.flash.activity_origin = mode;
        self.flash.activity_kind = kind;
        if kind != ActivityKind::Query {
            self.flash.error = None;
        }
        self.flash.ble_activity = mode == Mode::Ble;
        match Job::start(work) {
            Ok(job) => self.flash.job = Some(job),
            Err(error) => {
                self.flash.set_activity_error(error);
                self.flash.finish_job_progress(None);
            }
        }
    }

    pub(super) fn start_flash_async<F>(
        &mut self,
        work: impl FnOnce(Arc<AtomicBool>, crate::update::ProgressReporter) -> F + Send + 'static,
    ) where
        F: Future<Output = Result<Payload, String>> + Send + 'static,
    {
        if self.shutting_down {
            return;
        }
        self.start_flash_async_for(self.flash_mode(), ActivityKind::Preparation, work);
    }

    fn start_flash_async_for<F>(
        &mut self,
        mode: Mode,
        kind: ActivityKind,
        work: impl FnOnce(Arc<AtomicBool>, crate::update::ProgressReporter) -> F + Send + 'static,
    ) where
        F: Future<Output = Result<Payload, String>> + Send + 'static,
    {
        if self.shutting_down || self.flash.busy() {
            return;
        }
        self.flash.activity_origin = mode;
        self.flash.activity_kind = kind;
        if kind != ActivityKind::Query {
            self.flash.error = None;
        }
        self.flash.ble_activity = mode == Mode::Ble;
        self.flash.job = Some(Job::start_async(&self.async_io, work));
    }

    pub(super) fn scan_flash_volumes(&mut self) {
        if self.shutting_down || self.flash.volume_scan.is_some() || self.flash.busy() {
            return;
        }
        self.flash.next_scan = Instant::now() + Duration::from_secs(3);
        let info = self
            .flash
            .catalog
            .as_ref()
            .map(|catalog| catalog.info_filename.clone())
            .unwrap_or_else(|| "INFO_UF2.TXT".into());
        match Job::start(move |_, _| uf2::scan_with_info(&info).map(Payload::Volumes)) {
            Ok(job) => self.flash.volume_scan = Some(job),
            Err(error) => self.flash.set_error(Mode::Uf2, error),
        }
    }

    pub(super) fn release_flash_lease(&mut self) {
        self.flash
            .release_leases(&mut self.manager, &mut self.monitor);
    }

    pub(super) fn reserve_flash_action(&mut self, action: Action) {
        if self.shutting_down || self.flash.busy() {
            return;
        }
        if matches!(action, Action::Ble(..)) && !self.preferences.advanced_features {
            return;
        }
        self.flash.activity_origin = action.mode();
        if let Action::EsbGroups(groups) = &action
            && let Err(error) = self.validate_flash_ota_routes(groups)
        {
            self.flash.set_activity_error(error);
            self.flash.review = None;
            self.flash.acknowledged = false;
            self.flash.finish_ota_operation();
            return;
        }
        if let Action::Uf2(volumes, _, _) | Action::Reboot(volumes) = &action
            && (volumes.is_empty()
                || volumes.iter().any(|old| {
                    !self
                        .flash
                        .volumes
                        .iter()
                        .any(|live| live.identity == old.identity && live.mount == old.mount)
                }))
        {
            self.flash
                .set_activity_error(tr("firmware.error.select_target"));
            self.flash.review = None;
            return;
        }
        self.flash.activity_kind = ActivityKind::for_action(&action);
        if matches!(&action, Action::QueryGroups(_)) {
            self.flash.invalidated_query_receivers.clear();
        }
        self.flash.refresh_after_job = matches!(&action, Action::Uf2(..) | Action::Reboot(..));
        if !self.flash.querying() {
            self.flash.error = None;
        }
        self.flash.prepare_action_results(&action);
        if let Some(catalog) = &self.flash.catalog {
            match &action {
                Action::Uf2(volumes, _, _) => {
                    for volume in volumes {
                        if let Some(note) = catalog
                            .preset_for_volume(&volume.label, volume.board_id.as_deref())
                            .and_then(|preset| preset.success.as_ref())
                        {
                            self.flash
                                .result_notes
                                .insert(volume.identity.clone(), note.clone());
                        }
                    }
                }
                Action::OpenDfu(port, image, _) => {
                    if let Some(note) = catalog
                        .opendfu_targets
                        .iter()
                        .find(|target| image.resource.as_deref() == Some(&target.firmware))
                        .and_then(|target| target.success.as_ref())
                    {
                        self.flash
                            .result_notes
                            .insert(port.port_name.clone(), note.clone());
                    }
                }
                Action::Ble(devices, image) => {
                    if let Some(note) = catalog
                        .ble_ota_targets
                        .iter()
                        .find(|target| image.resource.as_deref() == Some(&target.package))
                        .and_then(|target| target.success.as_ref())
                    {
                        for device in devices {
                            self.flash
                                .result_notes
                                .insert(device.address.clone(), note.clone());
                        }
                    }
                }
                _ => {}
            }
        }
        let mut receivers = std::collections::BTreeMap::new();
        let mut addresses = BTreeSet::new();
        match &action {
            Action::QueryGroups(plan) => {
                for (receiver, trackers, _) in &plan.groups {
                    receivers.insert(receiver.key.clone(), receiver);
                    addresses.extend(
                        trackers
                            .iter()
                            .filter_map(|tracker| tracker.address.as_deref())
                            .filter_map(crate::domain::normalize_address),
                    );
                }
            }
            Action::EsbGroups(groups) => {
                for target in groups.iter().flat_map(|group| &group.targets) {
                    receivers.insert(target.receiver.key.clone(), &target.receiver);
                    addresses.extend(
                        target
                            .address
                            .as_deref()
                            .and_then(crate::domain::normalize_address),
                    );
                }
            }
            _ => {}
        }
        for receiver in receivers.values() {
            if let Some(observation) = self.monitor.observation(&receiver.key) {
                addresses.extend(
                    observation
                        .trackers
                        .iter()
                        .filter_map(|tracker| tracker.address.as_deref())
                        .filter_map(crate::domain::normalize_address),
                );
            }
        }
        let mut leases = Vec::new();
        let reservation = (|| -> Result<(), String> {
            match &action {
                Action::Enter(port) | Action::OpenDfu(port, _, _) => {
                    self.console_check_port(&port.port_name)?;
                    self.flash_check_port(&port.port_name)?;
                    if !self.manager.writes_idle(&port.port_name) {
                        return Err(tr("firmware.error.writes_pending"));
                    }
                    leases.push(Lease::Serial {
                        token: self.manager.reserve(&port.port_name, None)?,
                        port: port.port_name.clone(),
                    });
                }
                Action::QueryGroups(plan) => {
                    for (receiver, trackers, _) in &plan.groups {
                        self.flash_check_receiver_io(&receiver.key)?;
                        for tracker in trackers {
                            let address = tracker
                                .address
                                .as_deref()
                                .ok_or_else(|| tr("commands.route.error.address_unknown"))?;
                            self.flash_check_scoped_tracker_io(&receiver.key, address)?;
                        }
                        leases.push(Lease::Hid(self.monitor.reserve(receiver)?));
                    }
                }
                _ => {
                    for receiver in receivers.values() {
                        self.flash_check_receiver(&receiver.key)?;
                        leases.push(Lease::Hid(self.monitor.reserve(receiver)?));
                    }
                }
            }
            Ok(())
        })();
        self.flash.leases = leases;
        if let Err(error) = reservation {
            self.release_flash_lease();
            if let Action::QueryGroups(plan) = &action {
                for (receiver, trackers, include_receiver) in &plan.groups {
                    if *include_receiver {
                        let (key, failure) =
                            query_failure(receiver, esb_ota::RECEIVER_ID, error.clone());
                        self.flash.query_failures.insert(key, failure);
                    }
                    self.flash.query_failures.extend(
                        trackers
                            .iter()
                            .map(|tracker| query_failure(receiver, tracker.slot, error.clone())),
                    );
                }
            }
            self.flash.set_activity_error(error);
            self.flash.finish_job_progress(None);
            return;
        }
        self.flash.operation_pins = OperationPins::freeze(
            addresses,
            receivers.values().copied(),
            &self.manager,
            &self.inventory.ports,
        );
        self.flash.pending = Some(action);
    }

    pub(in crate::application) fn observe_flash_identities(
        &mut self,
        changes: &crate::sessions::SessionChanges,
    ) {
        if self.flash.leases.is_empty() {
            return;
        }
        self.flash
            .operation_pins
            .observe_changes(changes, &self.inventory.ports);
    }

    pub(in crate::application) fn poll_flash(&mut self) -> bool {
        if self.shutting_down {
            return false;
        }
        let mut changed = false;
        self.sync_esb_page();
        changed |= self.sync_flash_tracker_selection();
        if self.flash.pending.is_some() {
            let ready = self.flash.leases.iter().try_fold(true, |ready, lease| {
                let next = match lease {
                    Lease::Serial { token, .. } => self.manager.reservation_ready(token),
                    Lease::Hid(token) => self.monitor.reservation_ready(token),
                }?;
                Ok::<_, String>(ready && next)
            });
            let ready = ready.map(|ready| {
                ready
                    && self.manager.sessions().all(|session| {
                        self.flash_check_port(&session.port).is_ok()
                            || self.manager.writes_idle(&session.port)
                    })
            });
            match ready {
                Ok(true) => {
                    let action = self.flash.pending.take().unwrap();
                    let mode = action.mode();
                    let kind = ActivityKind::for_action(&action);
                    match action.into_request() {
                        firmware_update::ExecutionRequest::Blocking(request) => {
                            self.start_flash_job_for(mode, kind, move |cancel, progress| {
                                firmware_update::execute(request, cancel, progress)
                                    .map(Payload::Update)
                            });
                        }
                        firmware_update::ExecutionRequest::Ble(devices, image) => {
                            let backend = self.async_io.ble_backend();
                            self.start_flash_async_for(
                                mode,
                                kind,
                                move |cancel, reporter| async move {
                                    let mut progress = |value| reporter.emit(value);
                                    firmware_update::execute_ble(
                                        &backend,
                                        devices,
                                        image,
                                        &cancel,
                                        &mut progress,
                                    )
                                    .await
                                    .map(Payload::Update)
                                },
                            );
                        }
                    }
                    if self.flash.job.is_none() {
                        self.flash.invalidated_query_receivers.clear();
                        self.release_flash_lease();
                    }
                    changed = true;
                }
                Err(error) => {
                    self.flash.pending = None;
                    self.flash.invalidated_query_receivers.clear();
                    self.release_flash_lease();
                    self.flash.set_activity_error(error);
                    self.flash.finish_job_progress(None);
                    changed = true;
                }
                Ok(false) => {}
            }
        }
        if self.flash.job.is_some() {
            changed = true;
            if let Some((result, progress)) = self.flash.poll_job() {
                let query_activity = self.flash.activity_kind == ActivityKind::Query;
                self.release_flash_lease();
                self.flash.finish_job_progress(progress);
                match result {
                    Err(error) => {
                        if query_activity {
                            self.flash.query_error = Some(error);
                            self.flash.invalidated_query_receivers.clear();
                        } else {
                            self.flash.set_activity_error(error);
                        }
                    }
                    Ok(Payload::Catalog(catalog, volumes)) => {
                        self.flash.catalog_loaded = true;
                        match catalog {
                            Ok(catalog) => self.flash.catalog = Some(catalog),
                            Err(error) => {
                                self.flash.catalog_error = Some(error);
                                self.flash.catalog = Some(board_catalog::Catalog::default());
                            }
                        }
                        self.flash.apply_volumes(volumes);
                    }
                    Ok(Payload::Volumes(volumes)) => self.flash.apply_volumes(volumes),
                    Ok(Payload::Images(destination, images)) => {
                        self.flash.apply_images(destination, images);
                    }
                    Ok(
                        payload @ (Payload::Versions(..)
                        | Payload::Latest(..)
                        | Payload::Artifacts(..)
                        | Payload::RemoteImages(..)),
                    ) => {
                        let mode = self.flash_mode();
                        self.flash.apply_remote(mode, payload);
                    }
                    Ok(Payload::Ble(devices)) => {
                        self.flash.ble = devices;
                        self.flash.selected_ble.clear();
                    }
                    Ok(Payload::Update(completion)) => match completion {
                        firmware_update::UpdateCompletion::Queries(query) => {
                            self.flash
                                .apply_current_queries(query.into(), &self.inventory.receivers);
                        }
                        firmware_update::UpdateCompletion::Results(results) => {
                            self.flash.apply_joined_operation_results(
                                results
                                    .into_iter()
                                    .map(|(identity, result)| (identity.into(), result))
                                    .collect(),
                                Instant::now(),
                            );
                            self.flash.selected_port = None;
                        }
                    },
                }
            }
        }
        changed |= self.flash.poll_internal_default();
        if let Some(result) = self.flash.volume_scan.as_mut().and_then(Job::poll) {
            self.flash.volume_scan = None;
            match result {
                Ok(Payload::Volumes(volumes)) => self.flash.apply_volumes(volumes),
                Err(error) => self.flash.set_error(Mode::Uf2, error),
                _ => unreachable!(),
            }
            let interval = self
                .flash
                .catalog
                .as_ref()
                .map(|catalog| catalog.poll_interval_seconds)
                .filter(|value| value.is_finite() && *value > 0.)
                .unwrap_or(3.);
            self.flash.next_scan =
                Instant::now() + Duration::from_secs_f64(interval.clamp(1., 60.));
            changed = true;
        }
        let periodic_scan = (self.active_page == FeaturePage::Uf2 || self.flash.auto)
            && self.flash.catalog_loaded
            && Instant::now() >= self.flash.next_scan;
        if (periodic_scan || self.flash.refresh_after_job)
            && !self.flash.busy()
            && self.flash.volume_scan.is_none()
        {
            self.scan_flash_volumes();
            self.flash.refresh_after_job = false;
        }
        if self.flash.auto
            && !self.flash.busy()
            && self.flash.review.is_none()
            && self.flash.volume_scan.is_none()
            && let Some(image) = self.flash.image.clone()
        {
            let volumes: Vec<_> = self
                .flash
                .volumes
                .iter()
                .filter(|volume| {
                    self.flash.selected_volumes.contains(&volume.identity)
                        && !self
                            .flash
                            .auto_seen
                            .contains(&format!("{}:{}", volume.identity, image.digest))
                })
                .cloned()
                .collect();
            if !volumes.is_empty() {
                for volume in &volumes {
                    self.flash
                        .auto_seen
                        .insert(format!("{}:{}", volume.identity, image.digest));
                }
                match planning::plan_uf2_action(
                    self.flash.catalog.as_ref(),
                    self.flash.category,
                    volumes,
                    image,
                ) {
                    Ok(action) => self.reserve_flash_action(action),
                    Err(error) => self.flash.set_activity_error(error),
                }
                changed = true;
            }
        }
        let mut query_receivers = self
            .inventory
            .receivers
            .iter()
            .filter(|receiver| self.flash_check_receiver_io(&receiver.key).is_ok())
            .peekable();
        let available = query_receivers.peek().is_some()
            && query_receivers.all(|receiver| self.monitor.observation(&receiver.key).is_some());
        let blocked = self.flash.busy() || self.flash.review.is_some();
        if self
            .flash
            .esb_auto_query
            .take_ready(available, blocked, Instant::now())
        {
            self.query_flash_targets(true);
            changed = true;
        } else {
            let mut auto_query = std::mem::take(&mut self.flash.esb_auto_query);
            let groups = auto_query.take_incremental(
                self.inventory
                    .receivers
                    .iter()
                    .filter(|receiver| self.flash_check_receiver_io(&receiver.key).is_ok())
                    .filter(|receiver| self.monitor.observation(&receiver.key).is_some()),
                self.inventory
                    .receivers
                    .iter()
                    .filter_map(|receiver| self.monitor.observation(&receiver.key))
                    .flat_map(|observation| observation.trackers.iter())
                    .filter(|tracker| {
                        auto_query::calibration_query_tracker_admitted(
                            &self.calibration.session,
                            tracker,
                        )
                    }),
                blocked,
                Instant::now(),
            );
            self.flash.esb_auto_query = auto_query;
            if !groups.is_empty() {
                self.reserve_flash_action(Action::QueryGroups(OtaQueryPlan {
                    groups,
                    automatic: true,
                    incremental: true,
                }));
                changed = true;
            }
        }
        if self.flash.esb_auto_query.active && self.flash.review.is_none() {
            changed |= self.flash.start_internal_default();
        }
        changed
    }
}
