use super::*;

impl FlashState {
    fn group_selected(&self, key: &str) -> bool {
        self.targets.iter().any(|target| {
            self.selected_targets.contains(&ota_target_key(target))
                && ota_assignment_key(target) == key
        })
    }

    pub(super) fn reconcile_groups(&mut self, preferred: Option<String>) {
        let keys: BTreeSet<_> = self
            .targets
            .iter()
            .filter(|target| self.selected_targets.contains(&ota_target_key(target)))
            .map(ota_assignment_key)
            .collect();
        self.assignments.retain(|key, _| keys.contains(key));
        self.group_generations.retain(|key, _| keys.contains(key));
        for key in &keys {
            if !self.group_generations.contains_key(key) {
                self.next_group_generation += 1;
                self.group_generations
                    .insert(key.clone(), self.next_group_generation);
            }
        }
        self.ota_defaults
            .retain(|key, state| self.group_generations.get(key) == Some(&state.generation));
        for (key, generation) in &self.group_generations {
            if !self.ota_defaults.contains_key(key) {
                self.ota_defaults.insert(
                    key.clone(),
                    OtaDefault {
                        generation: *generation,
                        ..OtaDefault::default()
                    },
                );
            }
        }
        let active = preferred
            .filter(|key| keys.contains(key))
            .or_else(|| self.active_group.clone().filter(|key| keys.contains(key)))
            .or_else(|| keys.first().cloned());
        if active != self.active_group {
            self.latest_version = None;
            self.active_group = active;
            self.variant_filter = None;
            if self.error_origin == Mode::Esb {
                self.error = None;
            }
        }
    }

    pub(super) fn destination(&self, mode: Mode) -> Option<ImageDestination> {
        if mode != Mode::Esb {
            return Some(ImageDestination::Global);
        }
        let key = self.active_group.as_ref()?;
        Some(ImageDestination::Ota {
            key: key.clone(),
            generation: *self.group_generations.get(key)?,
        })
    }

    pub(super) fn destination_exists(&self, destination: &ImageDestination) -> bool {
        match destination {
            ImageDestination::Global => true,
            ImageDestination::Ota { key, generation } => {
                self.group_generations.get(key) == Some(generation) && self.group_selected(key)
            }
        }
    }

    pub(super) fn displayed_image(&self, mode: Mode) -> Option<&Image> {
        if mode == Mode::Esb {
            self.active_group
                .as_ref()
                .and_then(|key| self.assignments.get(key))
        } else {
            self.image.as_ref()
        }
    }

    pub(super) fn explicit_image_intent(&mut self, destination: &ImageDestination) {
        if !self.destination_exists(destination) {
            return;
        }
        if let ImageDestination::Ota { key, .. } = destination
            && let Some(state) = self.ota_defaults.get_mut(key)
        {
            state.explicit = true;
            state.error = None;
        }
    }

    pub(super) fn choose_image(
        &mut self,
        destination: &ImageDestination,
        image: Image,
    ) -> Result<(), String> {
        if !self.destination_exists(destination) {
            return Ok(());
        }
        self.explicit_image_intent(destination);
        self.assign_image(destination, image)?;
        self.review = None;
        self.acknowledged = false;
        self.auto = false;
        self.auto_requested = false;
        Ok(())
    }

    pub(super) fn assign_image(
        &mut self,
        destination: &ImageDestination,
        image: Image,
    ) -> Result<(), String> {
        if !self.destination_exists(destination) {
            return Ok(());
        }
        match destination {
            ImageDestination::Global => self.image = Some(image),
            ImageDestination::Ota { key, .. } => {
                for target in self.targets.iter().filter(|target| {
                    self.selected_targets.contains(&ota_target_key(target))
                        && ota_assignment_key(target) == *key
                }) {
                    validate_ota_image(&image, target)?;
                }
                self.assignments.insert(key.clone(), image);
            }
        }
        Ok(())
    }

    pub(super) fn apply_images(&mut self, destination: ImageDestination, mut images: Vec<Image>) {
        if !self.destination_exists(&destination) {
            return;
        }
        self.explicit_image_intent(&destination);
        if let Some(catalog) = &self.catalog {
            for image in &mut images {
                image.hints = catalog.match_filename(&image.name).into();
            }
        }
        self.review = None;
        self.acknowledged = false;
        self.auto = false;
        self.auto_requested = false;
        if images.len() == 1 && images[0].validation_error.is_none() {
            // Assignment validates before replacing the current image. A parsed
            // local file can still be incompatible with its frozen OTA group.
            if let Err(error) = self.choose_image(&destination, images[0].clone()) {
                self.set_error(self.activity_origin, error);
            }
        } else {
            match &destination {
                ImageDestination::Global => self.image = None,
                ImageDestination::Ota { key, .. } => {
                    self.assignments.remove(key);
                }
            }
        }
        self.images_destination = destination;
        self.images = images;
    }

    pub(super) fn internal_image_target(
        &self,
        key: &str,
    ) -> Result<&board_catalog::FlashTarget, String> {
        let target = self
            .targets
            .iter()
            .find(|target| {
                self.selected_targets.contains(&ota_target_key(target))
                    && ota_assignment_key(target) == key
            })
            .ok_or_else(|| tr("firmware.error.select_target"))?;
        let kind = if target.id == esb_ota::RECEIVER_ID {
            board_catalog::EsbOtaKind::Receiver
        } else {
            board_catalog::EsbOtaKind::Tracker
        };
        self.catalog
            .as_ref()
            .ok_or_else(|| tr("firmware.error.configuration_unavailable"))?
            .ota_target(kind, &target.info.board_target)
    }

    pub(super) fn internal_image_resource(&self, key: &str) -> Result<&Resource, String> {
        let target = self
            .targets
            .iter()
            .find(|target| {
                self.selected_targets.contains(&ota_target_key(target))
                    && ota_assignment_key(target) == key
            })
            .ok_or_else(|| tr("firmware.error.select_target"))?;
        self.internal_image_target(key)?
            .image_for_bootloader(&target.info.bootloader)
    }

    // One attempt per selected group generation, independent of focus and liveness.
    // Catalog loading and all file I/O happen outside rendering.
    pub(super) fn take_internal_default(
        &mut self,
    ) -> Option<(ImageDestination, Result<Resource, String>)> {
        if self.default_job.is_some() {
            return None;
        }
        self.catalog.as_ref()?;
        loop {
            let (key, state) = self.ota_defaults.iter_mut().find(|(key, state)| {
                !state.attempted && !state.explicit && !self.assignments.contains_key(*key)
            })?;
            state.attempted = true;
            let key = key.clone();
            let destination = ImageDestination::Ota {
                key: key.clone(),
                generation: state.generation,
            };
            let path = self.internal_image_resource(&key).cloned();
            if path.as_ref().is_ok_and(Resource::is_intentionally_omitted) {
                continue;
            }
            return Some((destination, path));
        }
    }

    pub(super) fn start_internal_default(&mut self) -> bool {
        let Some((destination, path)) = self.take_internal_default() else {
            return false;
        };
        match path {
            Ok(path) => match Job::start(move |cancel, _| load_image(path, cancel)) {
                Ok(job) => self.default_job = Some((destination, job)),
                Err(error) => self.apply_internal_default(destination, Err(error)),
            },
            Err(error) => self.apply_internal_default(destination, Err(error)),
        }
        true
    }

    pub(super) fn poll_internal_default(&mut self) -> bool {
        let Some((destination, _)) = self.default_job.as_ref() else {
            return false;
        };
        let obsolete = !self.destination_exists(destination)
            || matches!(destination, ImageDestination::Ota { key, .. }
                if self.ota_defaults.get(key).is_none_or(|state| state.explicit));
        let (_, job) = self.default_job.as_mut().unwrap();
        if obsolete {
            // Cancellation does not release the slot until the worker is joined.
            job.cancel();
        }
        let Some(result) = job.poll() else {
            return false;
        };
        let (destination, _) = self.default_job.take().unwrap();
        self.apply_internal_default(destination, result);
        true
    }

    pub(super) fn apply_internal_default(
        &mut self,
        destination: ImageDestination,
        result: Result<Image, String>,
    ) {
        if !self.destination_exists(&destination) {
            return;
        }
        let ImageDestination::Ota { ref key, .. } = destination else {
            return;
        };
        if self.assignments.contains_key(key)
            || self
                .ota_defaults
                .get(key)
                .is_none_or(|state| state.explicit)
        {
            return;
        }
        let result = result.and_then(|mut image| {
            if let Some(catalog) = &self.catalog {
                image.hints = catalog.match_filename(&image.name).into();
            }
            self.assign_image(&destination, image)
        });
        // A quiet OTA default must not revoke another page's confirmation or
        // UF2 auto-copy intent when its read finishes after navigation.
        if result.is_ok() && matches!(self.review, Some(Action::EsbGroups(_))) {
            self.review = None;
            self.acknowledged = false;
        }
        if let Some(state) = self.ota_defaults.get_mut(key) {
            state.error = result.err();
        }
    }
}
