use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use clap::ValueEnum;
use fff::frecency::FrecencyTracker;
use fff::{FilePicker, FilePickerOptions, SharedFilePicker, SharedFrecency};

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
pub(crate) enum IndexingMode {
    #[default]
    Auto,
    Eager,
    Lazy,
}

impl IndexingMode {
    pub(crate) fn eager(self, parent_age: Option<Duration>) -> bool {
        match self {
            Self::Auto => parent_age.is_some_and(|age| age < Duration::from_secs(60)),
            Self::Eager => true,
            Self::Lazy => false,
        }
    }
}

pub(crate) struct IndexStartup {
    picker: SharedFilePicker,
    options: Mutex<Option<(FilePickerOptions, Option<String>)>>,
    started: OnceLock<Result<(), String>>,
}

impl IndexStartup {
    pub(crate) fn new(
        picker: SharedFilePicker,
        options: FilePickerOptions,
        frecency_db_path: Option<String>,
    ) -> Self {
        Self {
            picker,
            options: Mutex::new(Some((options, frecency_db_path))),
            started: OnceLock::new(),
        }
    }

    pub(crate) fn start(&self) -> Result<(), String> {
        // OnceLock serializes first callers; the options lock is released before setup.
        self.started
            .get_or_init(|| {
                let (options, frecency_db_path) = self
                    .options
                    .lock()
                    .map_err(|e| format!("Failed to acquire index options: {e}"))?
                    .take()
                    .ok_or("Index options already consumed")?;
                let frecency = SharedFrecency::default();
                if let Some(path) = frecency_db_path {
                    match FrecencyTracker::open(&path) {
                        Ok(tracker) => {
                            let _ = frecency.init(tracker);
                        }
                        Err(error) => eprintln!("Warning: Failed to init frecency db: {error}"),
                    }
                }
                FilePicker::new_with_shared_state(self.picker.clone(), frecency, options)
                    .map_err(|e| format!("Failed to init file picker: {e}"))
            })
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_policy_handles_boundary_unknown_age_and_overrides() {
        for (age, eager) in [
            (None, false),
            (Some(Duration::ZERO), true),
            (Some(Duration::from_millis(59_999)), true),
            (Some(Duration::from_secs(60)), false),
            (Some(Duration::from_secs(600)), false),
        ] {
            assert_eq!(IndexingMode::Auto.eager(age), eager);
            assert!(IndexingMode::Eager.eager(age));
            assert!(!IndexingMode::Lazy.eager(age));
        }
    }
}
