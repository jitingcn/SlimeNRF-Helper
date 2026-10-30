/// Display-only choice; unclassified rows are always retained.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum LogFilter {
    #[default]
    All,
    Error,
    Warning,
    Info,
    Debug,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LogLevel {
    Error,
    Warning,
    Info,
    Debug,
}

impl LogFilter {
    pub(super) fn includes(self, level: Option<LogLevel>) -> bool {
        matches!(
            (self, level),
            (Self::All, _)
                | (_, None)
                | (Self::Error, Some(LogLevel::Error))
                | (Self::Warning, Some(LogLevel::Warning))
                | (Self::Info, Some(LogLevel::Info))
                | (Self::Debug, Some(LogLevel::Debug))
        )
    }
}

/// Recognizes a leading Zephyr level and module in already-decoded terminal text.
pub(super) fn classify(line: &str) -> Option<LogLevel> {
    let mut line = line.trim_start();
    if line.starts_with('[') {
        let (_, remaining) = line.split_once(']')?;
        line = remaining.trim_start();
    }
    let (level, remaining) = if let Some(remaining) = line.strip_prefix("<err>") {
        (LogLevel::Error, remaining)
    } else if let Some(remaining) = line.strip_prefix("<wrn>") {
        (LogLevel::Warning, remaining)
    } else if let Some(remaining) = line.strip_prefix("<inf>") {
        (LogLevel::Info, remaining)
    } else {
        (LogLevel::Debug, line.strip_prefix("<dbg>")?)
    };
    let (module, _) = remaining.trim_start().split_once(':')?;
    let module = module.trim();
    if module.is_empty()
        || !module
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        return None;
    }
    Some(level)
}

#[cfg(test)]
mod tests {
    use super::{LogFilter, LogLevel, classify};

    #[test]
    fn classifies_only_leading_zephyr_tags_with_a_valid_module() {
        for (line, expected) in [
            ("<err> module: fault", LogLevel::Error),
            ("[00:00:01.234] <wrn> app_1: caution", LogLevel::Warning),
            ("  <inf> x: info", LogLevel::Info),
            ("<dbg> mod42: debug", LogLevel::Debug),
            ("[prefix]<inf>module :", LogLevel::Info),
        ] {
            assert_eq!(classify(line), Some(expected), "{line:?}");
        }
        for line in [
            "",
            "\n",
            "error happened",
            "module: error",
            "response <err> module: error",
            "[unfinished <err> module: error",
            "<err",
            "<err> module",
            "<err>: no module",
            "<err> bad-module: no",
            "<err> 中文: no",
            "<error> module: no",
            "\x1b[31m<err> module: not decoded",
        ] {
            assert_eq!(classify(line), None, "{line:?}");
        }
    }

    #[test]
    fn each_filter_matches_exact_level_and_all_unclassified_rows() {
        for filter in [
            LogFilter::All,
            LogFilter::Error,
            LogFilter::Warning,
            LogFilter::Info,
            LogFilter::Debug,
        ] {
            assert!(filter.includes(None));
            for (level, exact) in [
                (LogLevel::Error, LogFilter::Error),
                (LogLevel::Warning, LogFilter::Warning),
                (LogLevel::Info, LogFilter::Info),
                (LogLevel::Debug, LogFilter::Debug),
            ] {
                assert_eq!(
                    filter.includes(Some(level)),
                    filter == LogFilter::All || filter == exact
                );
            }
        }
    }
}
