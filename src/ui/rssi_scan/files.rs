use super::{RssiWindow, ScanReport};
use crate::{i18n::tr, rssi_scan};
use gpui_kit::{Context, PathPromptOptions};
use std::{io::Read, path::Path, sync::Arc};

const MAX_IMPORT_BYTES: u64 = 262_144;

fn load_scan(path: &Path) -> Result<Arc<ScanReport>, String> {
    let file = std::fs::File::open(path).map_err(|error| error.to_string())?;
    let mut text = String::new();
    file.take(MAX_IMPORT_BYTES + 1)
        .read_to_string(&mut text)
        .map_err(|error| error.to_string())?;
    if text.len() as u64 > MAX_IMPORT_BYTES {
        return Err(tr("rssi.import.error.size_limit"));
    }
    rssi_scan::parse_log(&text)
}

impl RssiWindow {
    pub(super) fn choose_log(&mut self, cx: &mut Context<Self>) {
        if self.document.stopped || self.chooser.is_some() {
            return;
        }
        let dialog = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(tr("rssi.import.dialog.prompt").into()),
        });
        let executor = cx.background_executor().clone();
        let generation = self.document.begin_import();
        self.chooser = Some(cx.spawn(async move |this, cx| {
            let result = match dialog.await {
                Ok(Ok(Some(paths))) => {
                    if let Some(path) = paths.into_iter().next() {
                        Some(
                            executor
                                .spawn(async move {
                                    let source = path
                                        .file_name()
                                        .unwrap_or_default()
                                        .to_string_lossy()
                                        .into_owned();
                                    load_scan(&path).map(|report| (report, source))
                                })
                                .await,
                        )
                    } else {
                        None
                    }
                }
                Ok(Ok(None)) => None,
                Ok(Err(error)) => Some(Err(error.to_string())),
                Err(error) => Some(Err(error.to_string())),
            };
            let _ = this.update(cx, |this, cx| {
                this.chooser = None;
                this.document.finish_import(generation, result);
                if !this.document.stopped {
                    cx.notify();
                }
            });
        }));
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_import_is_bounded_and_uses_the_shared_parser() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("scan.log");
        let text = include_str!("../../rssi_scan/fixtures/receiver_scan.log");
        std::fs::write(&path, text).unwrap();
        assert_eq!(
            load_scan(&path).unwrap(),
            rssi_scan::parse_log(text).unwrap()
        );
        std::fs::write(&path, vec![b' '; MAX_IMPORT_BYTES as usize + 1]).unwrap();
        assert!(load_scan(&path).is_err());
    }
}
