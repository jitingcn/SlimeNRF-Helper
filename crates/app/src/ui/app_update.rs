use super::{Workbench, page_scroll::reveal_on_focus};
use crate::{
    app_update::{self, Channel, CheckResult, DownloadedUpdate, Release, ReleaseInfo},
    i18n::tr,
    update::Job,
};
use futures_util::{FutureExt as _, future::Shared};
use gpui_kit::base::{Link, TestSupportExt as _};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
};
use gpui_kit::{
    AnyElement, Context, FontWeight, ScrollHandle, SharedString, Task, Window, div, prelude::*,
};
#[cfg(test)]
use std::sync::atomic::Ordering;
use std::time::{SystemTime, UNIX_EPOCH};

const CHECK_INTERVAL_SECONDS: u64 = 24 * 60 * 60;

#[derive(Clone, Copy)]
enum ErrorPhase {
    Check,
    Download,
}

#[derive(Default)]
enum Status {
    #[default]
    Idle,
    SavingAttempt,
    Checking,
    Downloading,
    Cancelling,
    Current,
    NoStable,
    NoEligible,
    NoCompatible,
    Available,
    Ready,
    Cancelled,
    Busy,
    AutoSaveError,
    Error(ErrorPhase, String),
}

pub(super) enum Outcome {
    Checked(CheckResult),
    Downloaded(Box<DownloadedUpdate>),
}

enum SelectedRelease {
    Installable(Release),
    PageOnly(ReleaseInfo),
}

struct ReleaseDetails<'a> {
    version: &'a str,
    tag: &'a str,
    prerelease: bool,
    notes: &'a str,
    html_url: &'a str,
}

impl SelectedRelease {
    fn details(&self) -> ReleaseDetails<'_> {
        match self {
            Self::Installable(release) => ReleaseDetails {
                version: &release.version,
                tag: &release.tag,
                prerelease: release.prerelease,
                notes: &release.notes,
                html_url: &release.html_url,
            },
            Self::PageOnly(release) => ReleaseDetails {
                version: &release.version,
                tag: &release.tag,
                prerelease: release.prerelease,
                notes: &release.notes,
                html_url: &release.html_url,
            },
        }
    }

    fn installable(&self) -> Option<&Release> {
        match self {
            Self::Installable(release) => Some(release),
            Self::PageOnly(_) => None,
        }
    }
}

pub(super) struct AppUpdateState {
    installable: bool,
    automatic: bool,
    status: Status,
    job: Option<Job<Outcome>>,
    pending_auto_attempt: Option<u64>,
    cancelling: bool,
    navigation_locked: bool,
    progress: Option<(u64, u64)>,
    release: Option<SelectedRelease>,
    notes: Option<SharedString>,
    downloaded: Option<DownloadedUpdate>,
    file_cleanup: Vec<Shared<Task<()>>>,
    #[cfg(windows)]
    restart_update: Option<DownloadedUpdate>,
    #[cfg(test)]
    test_check: Option<TestCheck>,
    #[cfg(test)]
    test_download: Option<TestDownload>,
    #[cfg(test)]
    offline: bool,
}

#[cfg(test)]
enum TestCheck {
    Result(CheckResult),
    ByChannel {
        stable: CheckResult,
        preview: CheckResult,
    },
    WaitForCancel,
}

#[cfg(test)]
enum TestDownload {
    Success,
    Failure(String),
    WaitForCancel,
    GatedProgress {
        cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
        release: std::sync::mpsc::Receiver<()>,
    },
}

impl AppUpdateState {
    fn busy(&self) -> bool {
        self.job.is_some() || self.pending_auto_attempt.is_some()
    }

    pub(super) fn new() -> Self {
        let installable = app_update::installable_now();
        Self {
            installable,
            automatic: installable,
            status: Status::Idle,
            job: None,
            pending_auto_attempt: None,
            cancelling: false,
            navigation_locked: false,
            progress: None,
            release: None,
            notes: None,
            downloaded: None,
            file_cleanup: Vec::new(),
            #[cfg(windows)]
            restart_update: None,
            #[cfg(test)]
            test_check: None,
            #[cfg(test)]
            test_download: None,
            #[cfg(test)]
            offline: false,
        }
    }

    #[cfg(test)]
    pub(super) fn offline() -> Self {
        let mut state = Self::new();
        state.installable = false;
        state.offline = true;
        state.automatic = false;
        state
    }
}

pub(super) fn installed_version() -> SharedString {
    #[cfg(windows)]
    let prerelease = app_update::windows::installed_prerelease();
    #[cfg(not(windows))]
    let prerelease = false;
    version_label(env!("SLIMENRF_BUILD_DISPLAY_VERSION"), prerelease)
}

fn version_label(version: &'static str, prerelease: bool) -> SharedString {
    let semver_without_build = version.split_once('+').map_or(version, |(base, _)| base);
    if prerelease || semver_without_build.contains('-') {
        format!("{version} · {}", tr("app.settings.update.prerelease_badge")).into()
    } else {
        SharedString::new_static(version)
    }
}

fn unix_seconds() -> Option<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

fn check_due(last_attempt: Option<u64>, now: u64) -> bool {
    last_attempt.is_none_or(|last| now.saturating_sub(last) >= CHECK_INTERVAL_SECONDS)
}

impl Workbench {
    pub(super) fn app_update_navigation_locked(&self) -> bool {
        self.app_update.navigation_locked
    }

    fn discard_app_update(&mut self, downloaded: DownloadedUpdate, cx: &mut Context<Self>) {
        let cleanup = cx
            .background_executor()
            .spawn(async move {
                drop(downloaded);
            })
            .shared();
        self.app_update.file_cleanup.push(cleanup);
    }

    fn discard_current_app_update(&mut self, cx: &mut Context<Self>) {
        self.app_update.navigation_locked = false;
        if let Some(downloaded) = self.app_update.downloaded.take() {
            self.discard_app_update(downloaded, cx);
        }
    }

    pub(super) fn app_update_cleanup_files(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Vec<Shared<Task<()>>> {
        self.discard_current_app_update(cx);
        std::mem::take(&mut self.app_update.file_cleanup)
    }

    fn accept_app_update_release(&mut self, release: SelectedRelease) -> Status {
        let notes = release.details().notes;
        self.app_update.notes = (!notes.is_empty()).then(|| SharedString::from(notes));
        self.app_update.release = Some(release);
        Status::Available
    }

    fn start_app_update_check(&mut self, cx: &mut Context<Self>) {
        if self.shutting_down || self.app_update.busy() {
            return;
        }
        let Some(now) = unix_seconds() else {
            self.app_update.status = Status::Error(
                ErrorPhase::Check,
                "System clock is before the Unix epoch".into(),
            );
            cx.notify();
            return;
        };
        self.preferences.app_update_last_attempt = Some(now);
        self.save_preferences(cx);
        self.launch_app_update_check(cx);
    }

    fn queue_automatic_app_update_check(&mut self, now: u64, cx: &mut Context<Self>) {
        self.preferences.app_update_last_attempt = Some(now);
        self.app_update.pending_auto_attempt = Some(now);
        self.app_update.status = Status::SavingAttempt;
        self.save_preferences(cx);
    }

    fn launch_app_update_check(&mut self, cx: &mut Context<Self>) {
        self.app_update.release = None;
        self.app_update.notes = None;
        self.discard_current_app_update(cx);
        self.app_update.progress = None;
        self.app_update.cancelling = false;
        self.app_update.status = Status::Checking;
        let channel = if self.preferences.app_update_include_prereleases {
            Channel::Preview
        } else {
            Channel::Stable
        };
        #[cfg(test)]
        if let Some(next) = self.app_update.test_check.take() {
            self.app_update.job = match Job::start(move |cancel, _| match next {
                TestCheck::Result(result) => Ok(Outcome::Checked(result)),
                TestCheck::ByChannel { stable, preview } => Ok(Outcome::Checked(match channel {
                    Channel::Stable => stable,
                    Channel::Preview => preview,
                })),
                TestCheck::WaitForCancel => {
                    while !cancel.load(Ordering::Acquire) {
                        std::thread::sleep(std::time::Duration::from_millis(1));
                    }
                    Err("cancelled".into())
                }
            }) {
                Ok(job) => Some(job),
                Err(error) => {
                    self.app_update.status = Status::Error(ErrorPhase::Check, error);
                    None
                }
            };
            cx.notify();
            return;
        }
        #[cfg(test)]
        if self.app_update.offline {
            self.app_update.status = Status::Error(
                ErrorPhase::Check,
                "No offline check result was provided".into(),
            );
            cx.notify();
            return;
        }
        let executor = self.async_io.clone();
        self.app_update.job = Some(Job::start_async(
            &self.async_io,
            move |cancel, progress| async move {
                app_update::check(executor, channel, cancel, progress)
                    .await
                    .map(Outcome::Checked)
            },
        ));
        cx.notify();
    }

    fn start_app_update_download(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.shutting_down || !self.app_update.installable || self.app_update.busy() {
            return;
        }
        let Some(release) = self
            .app_update
            .release
            .as_ref()
            .and_then(SelectedRelease::installable)
            .cloned()
        else {
            return;
        };
        if !self.show_settings_about(window, cx) {
            return;
        }
        self.discard_current_app_update(cx);
        self.app_update.progress = None;
        self.app_update.cancelling = false;
        self.app_update.status = Status::Downloading;
        self.app_update.navigation_locked = true;
        #[cfg(test)]
        if let Some(next) = self.app_update.test_download.take() {
            self.app_update.job = match Job::start(move |cancel, progress| match next {
                TestDownload::Success => Ok(Outcome::Downloaded(Box::new(
                    app_update::test_downloaded_update(),
                ))),
                TestDownload::Failure(error) => Err(error),
                TestDownload::WaitForCancel => {
                    while !cancel.load(Ordering::Acquire) {
                        std::thread::sleep(std::time::Duration::from_millis(1));
                    }
                    Err("cancelled".into())
                }
                TestDownload::GatedProgress { cancelled, release } => {
                    progress(crate::update::Progress::bytes("fixture", 2, 8));
                    while !cancel.load(Ordering::Acquire) {
                        std::thread::sleep(std::time::Duration::from_millis(1));
                    }
                    cancelled.store(true, Ordering::Release);
                    let _ = release.recv();
                    Err("cancelled".into())
                }
            }) {
                Ok(job) => Some(job),
                Err(error) => {
                    self.app_update.navigation_locked = false;
                    self.app_update.status = Status::Error(ErrorPhase::Download, error);
                    None
                }
            };
            cx.notify();
            return;
        }
        #[cfg(test)]
        if self.app_update.offline {
            self.app_update.navigation_locked = false;
            self.app_update.status = Status::Error(
                ErrorPhase::Download,
                "No offline download result was provided".into(),
            );
            cx.notify();
            return;
        }
        let executor = self.async_io.clone();
        self.app_update.job = Some(Job::start_async(
            &self.async_io,
            move |cancel, progress| async move {
                app_update::download(executor, release, cancel, progress)
                    .await
                    .map(|downloaded| Outcome::Downloaded(Box::new(downloaded)))
            },
        ));
        cx.notify();
    }

    fn cancel_app_update(&mut self, cx: &mut Context<Self>) {
        if self.shutting_down {
            return;
        }
        if self.app_update.pending_auto_attempt.take().is_some() {
            self.app_update.status = Status::Cancelled;
        } else if let Some(job) = &self.app_update.job {
            job.cancel();
            self.app_update.cancelling = true;
            self.app_update.navigation_locked = false;
            self.app_update.status = Status::Cancelling;
        } else if self.app_update.downloaded.is_some() {
            self.discard_current_app_update(cx);
            self.app_update.status = Status::Cancelled;
        } else {
            return;
        }
        cx.notify();
    }

    fn app_update_hardware_busy(&self) -> bool {
        self.flash.busy() || self.capture.busy() || self.calibration.session.busy()
    }

    fn restart_app_update(&mut self, cx: &mut Context<Self>) {
        if self.shutting_down
            || self.app_update.busy()
            || !self.app_update.installable
            || self.app_update.downloaded.is_none()
        {
            return;
        }
        if self.app_update_hardware_busy() {
            self.app_update.status = Status::Busy;
            cx.notify();
        } else {
            #[cfg(windows)]
            {
                self.app_update.restart_update = self.app_update.downloaded.take();
                self.begin_shutdown(cx);
            }
        }
    }

    fn confirm_app_update_restart(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.shutting_down
            || self.app_update.busy()
            || !self.app_update.installable
            || self.app_update.downloaded.is_none()
        {
            return;
        }
        if self.app_update_hardware_busy() {
            self.app_update.status = Status::Busy;
            cx.notify();
            return;
        }
        // Kit buttons do not take pointer focus; the modal must restore its trigger.
        self.settings.app_update_restart_focus().focus(window, cx);
        let owner = cx.weak_entity();
        window.open_alert_dialog(cx, move |dialog, _, _| {
            let owner = owner.clone();
            dialog
                .title(tr("app.settings.update.confirm_title"))
                .description(tr("app.settings.update.confirm_body"))
                .confirm()
                .ok_text(tr("app.settings.update.confirm_action"))
                .cancel_text(tr("app.settings.update.cancel"))
                .on_ok(move |_, _, cx| {
                    let _ = owner.update(cx, |this, cx| this.restart_app_update(cx));
                    true
                })
        });
    }

    pub(super) fn poll_app_update(&mut self, cx: &mut Context<Self>) -> bool {
        self.poll_app_update_at(unix_seconds(), cx)
    }

    fn poll_app_update_at(&mut self, now: Option<u64>, cx: &mut Context<Self>) -> bool {
        self.app_update
            .file_cleanup
            .retain(|task| task.clone().now_or_never().is_none());
        let mut changed = false;
        let result = if let Some(job) = &mut self.app_update.job {
            let result = job.poll();
            if let Some(progress) = job.progress.take() {
                let next = Some((progress.completed, progress.total));
                if self.app_update.progress != next {
                    self.app_update.progress = next;
                    changed = true;
                }
            }
            result
        } else {
            None
        };
        if let Some(result) = result {
            self.app_update.job = None;
            let cancelling = self.app_update.cancelling;
            self.app_update.cancelling = false;
            changed = true;
            match result {
                Ok(Outcome::Downloaded(downloaded)) if cancelling => {
                    self.discard_app_update(*downloaded, cx);
                    self.app_update.status = Status::Cancelled;
                }
                _ if cancelling => {
                    self.app_update.status = Status::Cancelled;
                }
                Ok(Outcome::Checked(result)) => {
                    self.app_update.status = match result {
                        CheckResult::Available(release) => {
                            self.accept_app_update_release(SelectedRelease::Installable(*release))
                        }
                        CheckResult::ReleasePage(release) => {
                            self.accept_app_update_release(SelectedRelease::PageOnly(*release))
                        }
                        CheckResult::UpToDate => Status::Current,
                        CheckResult::NoCompatibleRelease => Status::NoCompatible,
                        CheckResult::NoEligibleRelease
                            if !self.preferences.app_update_include_prereleases =>
                        {
                            Status::NoStable
                        }
                        CheckResult::NoEligibleRelease => Status::NoEligible,
                    };
                }
                Ok(Outcome::Downloaded(downloaded)) => {
                    self.app_update.downloaded = Some(*downloaded);
                    self.app_update.status = Status::Ready;
                }
                Err(error) => {
                    self.app_update.navigation_locked = false;
                    let phase = match self.app_update.status {
                        Status::Downloading => ErrorPhase::Download,
                        _ => ErrorPhase::Check,
                    };
                    self.app_update.status = Status::Error(phase, error);
                }
            }
        }
        if let Some(attempt) = self.app_update.pending_auto_attempt {
            if self.shutting_down
                || !self.app_update.automatic
                || !self.preferences.app_update_auto_check
                || self.preferences.app_update_last_attempt != Some(attempt)
                || self.app_update.job.is_some()
                || self.app_update.downloaded.is_some()
            {
                self.app_update.pending_auto_attempt = None;
                self.app_update.status = Status::Cancelled;
                return true;
            }
            if !self.preferences_io.busy() {
                self.app_update.pending_auto_attempt = None;
                if self.preferences_io.latest_snapshot_saved(&self.preferences) {
                    self.launch_app_update_check(cx);
                } else if self.preferences_io.save_error.is_some() {
                    self.app_update.status = Status::AutoSaveError;
                } else {
                    self.app_update.status = Status::Cancelled;
                }
                return true;
            }
            return changed;
        }
        if !self.shutting_down
            && self.app_update.automatic
            && self.preferences.app_update_auto_check
            && self.preferences_io.load_error.is_none()
            && self.preferences_io.save_error.is_none()
            && self.app_update.job.is_none()
            && self.app_update.downloaded.is_none()
            && let Some(now) = now
            && check_due(self.preferences.app_update_last_attempt, now)
        {
            self.queue_automatic_app_update_check(now, cx);
            changed = true;
        }
        changed
    }

    #[cfg(windows)]
    pub(super) fn shutdown_app_update(
        &mut self,
    ) -> (Option<Job<Outcome>>, Option<DownloadedUpdate>) {
        let job = self.app_update.job.take();
        if let Some(job) = &job {
            job.cancel();
        }
        (job, self.app_update.restart_update.take())
    }

    #[cfg(not(windows))]
    pub(super) fn shutdown_app_update(&mut self) -> Option<Job<Outcome>> {
        let job = self.app_update.job.take();
        if let Some(job) = &job {
            job.cancel();
        }
        job
    }

    fn app_update_status(&self) -> String {
        match &self.app_update.status {
            Status::Idle => tr("app.settings.update.status_idle"),
            Status::SavingAttempt => tr("app.settings.update.status_saving_attempt"),
            Status::Checking => tr("app.settings.update.status_checking"),
            Status::Downloading => {
                let (completed, total) = self.app_update.progress.unwrap_or_default();
                rust_i18n::t!(
                    "app.settings.update.status_downloading",
                    completed = completed,
                    total = total
                )
                .to_string()
            }
            Status::Cancelling => tr("app.settings.update.status_cancelling"),
            Status::Current => tr("app.settings.update.status_current"),
            Status::NoStable => tr("app.settings.update.status_no_stable"),
            Status::NoEligible => tr("app.settings.update.status_no_eligible"),
            Status::NoCompatible => tr("app.settings.update.status_no_compatible"),
            Status::Available | Status::Ready => {
                let Some(release) = &self.app_update.release else {
                    return tr("app.settings.update.status_idle");
                };
                let release = release.details();
                if matches!(self.app_update.status, Status::Ready) {
                    rust_i18n::t!(
                        "app.settings.update.status_ready",
                        version = release.version,
                        tag = release.tag
                    )
                    .to_string()
                } else {
                    rust_i18n::t!(
                        "app.settings.update.status_available",
                        version = release.version,
                        tag = release.tag
                    )
                    .to_string()
                }
            }
            Status::Cancelled => tr("app.settings.update.status_cancelled"),
            Status::Busy => tr("app.settings.update.busy"),
            Status::AutoSaveError => tr("app.settings.update.auto_save_error"),
            Status::Error(phase, error) => match phase {
                ErrorPhase::Check => {
                    rust_i18n::t!("app.settings.update.check_error", error = error).to_string()
                }
                ErrorPhase::Download => {
                    rust_i18n::t!("app.settings.update.download_error", error = error).to_string()
                }
            },
        }
    }

    pub(super) fn render_app_update_controls(
        &self,
        scroll: &ScrollHandle,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut content = div()
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .items_stretch()
            .gap_3();
        let running = self.app_update.job.is_some() || self.shutting_down;
        let busy = running || self.app_update.pending_auto_attempt.is_some();
        let release_only = !self.app_update.installable
            || matches!(
                self.app_update.release.as_ref(),
                Some(SelectedRelease::PageOnly(_))
            );
        content = content
            .when(self.app_update.automatic, |content| {
                content.child(reveal_on_focus(
                    "app-update-auto-check-focus",
                    Checkbox::new("app-update-auto-check")
                        .w_full()
                        .min_w_0()
                        .label(tr("app.settings.update.automatic"))
                        .checked(self.preferences.app_update_auto_check)
                        .disabled(running)
                        .on_click(cx.listener(|this, checked: &bool, window, cx| {
                            if this.shutting_down
                                || this.app_update.job.is_some()
                                || !this.app_update.automatic
                            {
                                return;
                            }
                            let mut preferences = this.preferences.clone();
                            preferences.app_update_auto_check = *checked;
                            this.update_preferences(preferences, window, cx);
                            if !this.preferences.app_update_auto_check {
                                this.cancel_app_update(cx);
                            }
                        })),
                    scroll,
                ))
            })
            .child(reveal_on_focus(
                "app-update-prereleases-focus",
                Checkbox::new("app-update-prereleases")
                    .w_full()
                    .min_w_0()
                    .label(tr("app.settings.update.prereleases"))
                    .checked(self.preferences.app_update_include_prereleases)
                    .disabled(running)
                    .on_click(cx.listener(|this, checked: &bool, window, cx| {
                        if this.shutting_down || this.app_update.job.is_some() {
                            return;
                        }
                        let mut preferences = this.preferences.clone();
                        preferences.app_update_include_prereleases = *checked;
                        this.update_preferences(preferences, window, cx);
                        this.app_update.release = None;
                        this.app_update.notes = None;
                        this.discard_current_app_update(cx);
                        if this.app_update.pending_auto_attempt.is_none() {
                            this.app_update.status = Status::Idle;
                        }
                        cx.notify();
                    })),
                scroll,
            ))
            .child(
                div()
                    .text_sm()
                    .whitespace_normal()
                    .text_color(cx.theme().muted_foreground)
                    .child(tr(if !release_only {
                        "app.settings.update.trust"
                    } else {
                        "app.settings.update.trust_check_only"
                    })),
            )
            .when(release_only, |content| {
                content.child(
                    div()
                        .text_sm()
                        .whitespace_normal()
                        .child(tr("app.settings.update.check_only")),
                )
            });
        let message = self.app_update_status();
        content = content.child(
            div()
                .id("app-update-status")
                .anchor_scroll(Some(self.settings.app_update_scroll_anchor()))
                .role(gpui_kit::Role::Status)
                .test_support()
                .aria_label(message.clone())
                .w_full()
                .min_w_0()
                .text_sm()
                .whitespace_normal()
                .child(message),
        );
        if self.app_update_navigation_locked() {
            content = content.child(
                div()
                    .text_sm()
                    .whitespace_normal()
                    .text_color(cx.theme().muted_foreground)
                    .child(tr("app.settings.update.navigation_locked")),
            );
        }
        let mut actions = div().w_full().min_w_0().flex().flex_wrap().gap_2();
        actions = actions.child(reveal_on_focus(
            "app-update-check-focus",
            Button::new("app-update-check")
                .label(tr("app.settings.update.check"))
                .disabled(busy)
                .on_click(cx.listener(|this, _, _, cx| this.start_app_update_check(cx))),
            scroll,
        ));
        if self.app_update.installable
            && self
                .app_update
                .release
                .as_ref()
                .and_then(SelectedRelease::installable)
                .is_some()
            && self.app_update.downloaded.is_none()
        {
            actions = actions.child(reveal_on_focus(
                "app-update-download-focus",
                Button::new("app-update-download")
                    .label(tr("app.settings.update.download"))
                    .disabled(busy)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.start_app_update_download(window, cx)
                    })),
                scroll,
            ));
        }
        if self.app_update.busy() || self.app_update.downloaded.is_some() {
            actions = actions.child(reveal_on_focus(
                "app-update-cancel-focus",
                Button::new("app-update-cancel")
                    .label(tr("app.settings.update.cancel"))
                    .disabled(self.app_update.cancelling)
                    .on_click(cx.listener(|this, _, _, cx| this.cancel_app_update(cx))),
                scroll,
            ));
        }
        if self.app_update.downloaded.is_some() {
            actions = actions.child(reveal_on_focus(
                "app-update-restart-focus",
                Button::new("app-update-restart")
                    .track_focus(self.settings.app_update_restart_focus())
                    .label(tr("app.settings.update.restart"))
                    .primary()
                    .disabled(busy || !self.app_update.installable)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.confirm_app_update_restart(window, cx)
                    })),
                scroll,
            ));
        }
        content = content.child(actions);
        if let Some(release) = &self.app_update.release {
            let release = release.details();
            let label = tr("app.settings.update.release_link");
            content = content
                .when(release.prerelease, |content| {
                    let badge = tr("app.settings.update.prerelease_badge");
                    content.child(
                        div()
                            .id("app-update-prerelease")
                            .role(gpui_kit::Role::Label)
                            .test_support()
                            .aria_label(badge.clone())
                            .text_sm()
                            .child(badge),
                    )
                })
                .child(reveal_on_focus(
                    "app-update-release-link-focus",
                    div().w_full().min_w_0().flex().items_start().child(
                        div()
                            .id("app-update-release-link")
                            .test_support()
                            .min_w_0()
                            .max_w_full()
                            .child(
                                Link::new("release-link")
                                    .href(release.html_url)
                                    .accessibility_label(label.clone())
                                    .open_with(|url, _, _, cx| cx.open_url(url))
                                    .min_w_0()
                                    .max_w_full()
                                    .px_1()
                                    .py_1()
                                    .text_sm()
                                    .whitespace_normal()
                                    .text_color(cx.theme().link)
                                    .text_decoration_1()
                                    .text_decoration_color(cx.theme().link.opacity(0.5))
                                    .border_1()
                                    .border_color(cx.theme().border.opacity(0.))
                                    .rounded_sm()
                                    .cursor_pointer()
                                    .hover(|style| {
                                        style
                                            .text_color(cx.theme().link.opacity(0.8))
                                            .text_decoration_color(cx.theme().link)
                                    })
                                    .focus_visible(|style| style.border_color(cx.theme().ring))
                                    .child(label),
                            ),
                    ),
                    scroll,
                ));
            if let Some(notes) = &self.app_update.notes {
                content = content.child(
                    div()
                        .w_full()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(tr("app.settings.update.release_notes")),
                        )
                        .child(
                            div()
                                .id("app-update-release-notes")
                                .role(gpui_kit::Role::Label)
                                .test_support()
                                .aria_label(notes.clone())
                                .text_sm()
                                .whitespace_normal()
                                .child(notes.clone()),
                        ),
                );
            }
        }
        content.into_any_element()
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod cadence_tests;
