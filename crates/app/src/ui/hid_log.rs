use super::{FeaturePage, Workbench};
use crate::{
    hid_log::decode::{self, Category, DecodedRecord},
    hid_log::{self as trace, Record, RecordData, Recorder, Summary, TraceItem},
    i18n::tr,
};
use gpui_kit::component::{
    ActiveTheme, Disableable, ResizableState, Selectable, Sizable,
    button::{Button, ButtonVariants},
    checkbox::Checkbox,
    menu::{DropdownMenu, PopupMenuItem},
    resizable_panel,
};
use gpui_kit::{prelude::*, *};
use std::{
    collections::{BTreeSet, VecDeque},
    path::PathBuf,
    sync::Arc,
    thread::JoinHandle,
};

mod controller;
mod filters;
mod render;
#[cfg(test)]
mod tests;

const SENSOR_ROWS: usize = 2048;
const OTHER_ROWS: usize = 2048;
const RECENT_REPORTS: usize = 1024;
const EVENT_ROWS: usize = 2048;

struct Row {
    record: Arc<Record>,
    decoded: DecodedRecord,
}

pub(super) struct HidLogUiState {
    subscription: Option<trace::Subscription>,
    sensors: VecDeque<Arc<Row>>,
    others: VecDeque<Arc<Row>>,
    events: VecDeque<Arc<Row>>,
    reports: VecDeque<Arc<Record>>,
    visible: Vec<Arc<Row>>,
    dirty: bool,
    selected: Option<Arc<Row>>,
    receiver_filter: BTreeSet<String>,
    tracker_filter: BTreeSet<u8>,
    category_filter: Vec<Category>,
    kind_filter: BTreeSet<u8>,
    watch_receiver: Option<String>,
    layout: Option<Entity<ResizableState>>,
    scroll: UniformListScrollHandle,
    list_focus: Option<FocusHandle>,
    follow: bool,
    paused: bool,
    dropped: u64,
    evicted: u64,
    recorder: Option<Recorder>,
    recording_path: Option<PathBuf>,
    stopping: bool,
    chooser: Option<Task<()>>,
    file_worker: Option<JoinHandle<Result<FileResult, String>>>,
    error: Option<String>,
    notice: Option<String>,
}

impl Default for HidLogUiState {
    fn default() -> Self {
        Self {
            subscription: None,
            sensors: VecDeque::new(),
            others: VecDeque::new(),
            events: VecDeque::new(),
            reports: VecDeque::new(),
            visible: Vec::new(),
            selected: None,
            receiver_filter: BTreeSet::new(),
            tracker_filter: BTreeSet::new(),
            category_filter: vec![Category::Event],
            kind_filter: BTreeSet::new(),
            watch_receiver: None,
            layout: None,
            scroll: UniformListScrollHandle::default(),
            list_focus: None,
            follow: true,
            paused: false,
            dirty: true,
            dropped: 0,
            evicted: 0,
            recorder: None,
            recording_path: None,
            stopping: false,
            chooser: None,
            file_worker: None,
            error: None,
            notice: None,
        }
    }
}

impl HidLogUiState {
    fn can_start_recording(&self) -> bool {
        self.recorder.is_none()
            && !self.stopping
            && self.chooser.is_none()
            && self.file_worker.is_none()
    }

    fn can_stop_recording(&self) -> bool {
        self.recorder.is_some() && !self.stopping
    }

    fn stop_recording(&mut self) -> bool {
        if !self.can_stop_recording() {
            return false;
        }
        self.recorder.as_ref().expect("owned HID recorder").stop();
        self.stopping = true;
        true
    }

    fn poll_file_results(&mut self) -> bool {
        let mut changed = false;
        if self
            .file_worker
            .as_ref()
            .is_some_and(|worker| worker.is_finished())
        {
            let result = self
                .file_worker
                .take()
                .expect("finished HID file worker")
                .join();
            match result {
                Ok(Ok((summary, path))) => {
                    self.notice = Some(controller::saved_notice(&summary, &path));
                }
                Ok(Err(error)) => self.error = Some(error),
                Err(_) => self.error = Some(tr("hid.error.worker_stopped")),
            }
            changed = true;
        }
        if self.recorder.as_ref().is_some_and(Recorder::is_finished) {
            let recorder = self.recorder.take().expect("finished HID recorder");
            match recorder.join() {
                Ok(summary) => {
                    if let Some(path) = &self.recording_path {
                        self.notice = Some(controller::saved_notice(&summary, path));
                    }
                }
                Err(error) => self.error = Some(error),
            }
            self.stopping = false;
            self.recording_path = None;
            changed = true;
        }
        changed
    }

    fn toggle_category(&mut self, category: Category) {
        if let Some(index) = self
            .category_filter
            .iter()
            .position(|selected| *selected == category)
        {
            self.category_filter.remove(index);
        } else {
            self.category_filter.push(category);
        }
        if !self.category_filter.contains(&Category::Event) {
            self.kind_filter.clear();
        }
    }

    fn toggle_kind(&mut self, kind: u8) {
        if !self.kind_filter.remove(&kind) {
            self.kind_filter.insert(kind);
            if !self.category_filter.contains(&Category::Event) {
                self.category_filter.push(Category::Event);
            }
        }
    }

    fn selected_visible_index(&self) -> Option<usize> {
        let selected = self.selected.as_ref()?;
        self.visible.iter().position(|row| {
            row.record.sequence == selected.record.sequence
                && row.decoded.byte_offset == selected.decoded.byte_offset
        })
    }

    fn navigate_rows(&mut self, key: &str, page_rows: usize) -> bool {
        let Some(last) = self.visible.len().checked_sub(1) else {
            return false;
        };
        let current = self.selected_visible_index();
        let page_rows = page_rows.max(1);
        let index = match key {
            "up" => current.map_or(last, |index| index.saturating_sub(1)),
            "down" => current.map_or(0, |index| index.saturating_add(1).min(last)),
            "home" => 0,
            "end" => last,
            "pageup" => current.map_or(last, |index| index.saturating_sub(page_rows)),
            "pagedown" => current.map_or(0, |index| index.saturating_add(page_rows).min(last)),
            "enter" | "space" => current.unwrap_or(0),
            _ => return false,
        };
        if current != Some(index) {
            self.selected = Some(self.visible[index].clone());
        }
        self.follow = false;
        self.scroll.scroll_to_item(index, ScrollStrategy::Nearest);
        true
    }

    fn rebuild(&mut self) {
        if self.paused {
            return;
        }
        self.dirty = false;
        self.visible.clear();
        self.visible.extend(
            self.sensors
                .iter()
                .chain(self.others.iter())
                .chain(self.events.iter())
                .filter(|row| {
                    (self.receiver_filter.is_empty()
                        || self
                            .receiver_filter
                            .contains(&row.record.source.receiver_key))
                        && (self.tracker_filter.is_empty()
                            || row
                                .decoded
                                .tracker_id
                                .is_some_and(|id| self.tracker_filter.contains(&id)))
                        && (self.category_filter.is_empty()
                            || self.category_filter.contains(&row.decoded.category))
                        && (self.kind_filter.is_empty()
                            || row
                                .decoded
                                .event_kind
                                .is_some_and(|kind| self.kind_filter.contains(&kind)))
                })
                .cloned(),
        );
        self.visible
            .sort_unstable_by_key(|row| (row.record.sequence, row.decoded.byte_offset));
        if self.follow && !self.visible.is_empty() {
            self.scroll
                .scroll_to_item(self.visible.len() - 1, ScrollStrategy::Bottom);
        }
    }

    fn retain(&mut self, record: Arc<Record>) {
        let decoded = decode::decode(&record);
        self.dirty = true;
        self.reports.push_back(record.clone());
        if self.reports.len() > RECENT_REPORTS {
            self.reports.pop_front();
        }
        for decoded in decoded {
            let (rows, capacity) = match decoded.category {
                Category::Sensor => (&mut self.sensors, SENSOR_ROWS),
                Category::Event => (&mut self.events, EVENT_ROWS),
                _ => (&mut self.others, OTHER_ROWS),
            };
            rows.push_back(Arc::new(Row {
                record: record.clone(),
                decoded,
            }));
            if rows.len() > capacity {
                rows.pop_front();
                self.evicted += 1;
            }
        }
    }

    fn retained_rows(&self) -> impl Iterator<Item = &Arc<Row>> {
        self.sensors
            .iter()
            .chain(self.others.iter())
            .chain(self.events.iter())
            .chain(self.visible.iter())
            .chain(self.selected.iter())
    }

    fn retained_records(&self) -> impl Iterator<Item = &Arc<Record>> {
        self.reports
            .iter()
            .chain(self.retained_rows().map(|row| &row.record))
    }

    fn snapshot(&self) -> Vec<Arc<Record>> {
        let mut records: Vec<_> = self.retained_records().cloned().collect();
        records.sort_unstable_by_key(|record| record.sequence);
        records.dedup_by_key(|record| record.sequence);
        records
    }
}

type FileResult = (Summary, PathBuf);

pub(super) struct HidLogCleanup {
    recorder: Option<Recorder>,
    file_worker: Option<JoinHandle<Result<FileResult, String>>>,
}

impl HidLogCleanup {
    pub(super) fn join(self) {
        if let Some(recorder) = self.recorder {
            let _ = recorder.join();
        }
        if let Some(worker) = self.file_worker {
            let _ = worker.join();
        }
    }
}
