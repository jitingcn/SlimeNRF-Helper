use super::{AnalysisInput, TemperatureWindow, full_range, plot};
use crate::i18n::tr;
use futures_util::FutureExt as _;
use gpui_kit::{Context, PathPromptOptions};
use slimenrf_recording::thermal_dump::{self, ThermalDump};
use std::{io::Read, path::PathBuf};

// Imports are analysis documents, not firmware's lossless TCL2 backup format.
const MAX_IMPORT_BYTES: u64 = 262_144;

pub(super) fn load_dump(path: &std::path::Path) -> Result<ThermalDump, String> {
    let file = std::fs::File::open(path).map_err(|error| error.to_string())?;
    let mut text = String::new();
    file.take(MAX_IMPORT_BYTES + 1)
        .read_to_string(&mut text)
        .map_err(|error| error.to_string())?;
    if text.len() as u64 > MAX_IMPORT_BYTES {
        return Err(tr("temperature.import.error.size_limit"));
    }
    thermal_dump::parse_text(&text).map_err(|error| error.to_string())
}

pub(super) fn save_svg(
    path: PathBuf,
    analysis: &slimenrf_recording::thermal_dump::ThermalDumpAnalysis,
    range: [f64; 2],
) -> Result<PathBuf, String> {
    crate::file_export::write(&path, plot::svg(analysis, range).as_bytes())
        .map(|()| path)
        .map_err(|error| error.to_string())
}

impl TemperatureWindow {
    pub(super) fn choose_dump(&mut self, cx: &mut Context<Self>) {
        if self.document.stopped || self.chooser.is_some() || self.document.busy {
            return;
        }
        let dialog = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(tr("temperature.import.dialog.prompt").into()),
        });
        let generation = self.document.generation;
        self.chooser = Some(cx.spawn(async move |this, cx| {
            let result = dialog.await;
            let _ = this.update(cx, |this, cx| {
                this.chooser = None;
                if this.document.stopped || generation != this.document.generation {
                    cx.notify();
                    return;
                }
                match result {
                    Ok(Ok(Some(paths))) => {
                        if let Some(path) = paths.into_iter().next() {
                            let source = path.display().to_string();
                            this.queue_analysis(AnalysisInput::File(path), source, cx);
                        }
                    }
                    Ok(Ok(None)) => {}
                    Ok(Err(error)) => this.document.error = Some(error.to_string()),
                    Err(error) => this.document.error = Some(error.to_string()),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    pub(super) fn export_svg(&mut self, cx: &mut Context<Self>) {
        if self.document.stopped || self.export.is_some() {
            return;
        }
        // Pin analysis and viewport before opening the native dialog.
        let Some(analysis) = self.document.analysis.clone() else {
            return;
        };
        let range = self.document.range.unwrap_or_else(|| full_range(&analysis));
        let directory = directories::BaseDirs::new()
            .map(|dirs| dirs.home_dir().to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        let dialog = cx.prompt_for_new_path(&directory, Some("tcal-analysis.svg"));
        self.export = Some(cx.spawn(async move |this, cx| {
            let result = dialog.await;
            let _ = this.update(cx, |this, cx| {
                this.export = None;
                if this.document.stopped {
                    return;
                }
                match result {
                    Ok(Ok(Some(path))) => {
                        let result = std::sync::Arc::new(parking_lot::Mutex::new(None));
                        let published = result.clone();
                        let join = cx
                            .background_executor()
                            .spawn(async move {
                                *published.lock() = Some(save_svg(path, &analysis, range));
                            })
                            .shared();
                        this.work.export = Some(join.clone());
                        this.export = Some(cx.spawn(async move |this, cx| {
                            join.await;
                            let _ = this.update(cx, |this, cx| {
                                this.work.export = None;
                                this.export = None;
                                if this.document.stopped {
                                    return;
                                }
                                match result.lock().take() {
                                    Some(Ok(path)) => {
                                        this.document.status = Some(format!(
                                            "{}: {}",
                                            tr("temperature.export.status.saved"),
                                            path.display()
                                        ))
                                    }
                                    Some(Err(error)) => this.document.error = Some(error),
                                    None => {}
                                }
                                cx.notify();
                            });
                        }));
                    }
                    Ok(Ok(None)) => {}
                    Ok(Err(error)) => this.document.error = Some(error.to_string()),
                    Err(error) => this.document.error = Some(error.to_string()),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offline_import_uses_the_same_analysis_as_received_dump() {
        let text = "Dumping 2 collected temperature calibration points:\n2 | -4.00 | 0.01000 | 0.02000 | 0.03000\n8 | 12.00 | 0.02000 | 0.03000 | 0.04000\nEnd of dump. Total points printed: 2\n";
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("dump.txt");
        std::fs::write(&path, text).unwrap();
        let imported = thermal_dump::analyze(&load_dump(&path).unwrap());
        let received = thermal_dump::analyze(&thermal_dump::parse_text(text).unwrap());
        assert_eq!(imported.temperature_range_c, received.temperature_range_c);
        for (imported, received) in imported.axes.iter().zip(&received.axes) {
            assert_eq!(imported.raw, received.raw);
            assert_eq!(imported.linear, received.linear);
            assert_eq!(imported.mls, received.mls);
        }
    }

    #[test]
    fn offline_import_rejects_excessive_or_incomplete_files() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("dump.txt");
        std::fs::write(&path, vec![b' '; MAX_IMPORT_BYTES as usize + 1]).unwrap();
        assert!(load_dump(&path).is_err());
        std::fs::write(
            &path,
            "Dumping 1 collected temperature calibration points:\n0 | 1 | 2 | 3 | 4\n",
        )
        .unwrap();
        assert!(load_dump(&path).is_err());
    }
}
