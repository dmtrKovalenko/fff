use std::path::Path;

use super::FilePicker;
use crate::types::FileItem;

pub(super) enum RestoreChange {
    Upsert(String, Option<FileItem>),
    Remove(String),
    RemoveDirs(Vec<String>),
}

impl FilePicker {
    pub(crate) fn is_restoring_base(&self) -> bool {
        self.restore_changes.is_some()
    }

    pub(crate) fn record_restore_changes(&mut self) {
        self.restore_changes = Some(Vec::new());
    }

    pub(super) fn record_restore_path(
        &mut self,
        path: &Path,
        change: impl FnOnce(String) -> RestoreChange,
    ) {
        if self.is_restoring_base()
            && let Some(relative) = self.to_relative_path(path).map(|path| path.into_owned())
            && let Some(changes) = &mut self.restore_changes
        {
            changes.push(change(relative));
        }
    }

    pub(super) fn record_restore_upsert(&mut self, path: &Path) {
        self.record_restore_path(path, |path| RestoreChange::Upsert(path, None));
    }

    pub(super) fn take_restore_changes(&mut self) -> Vec<RestoreChange> {
        let mut changes = self.restore_changes.take().unwrap_or_default();
        for change in &mut changes {
            if let RestoreChange::Upsert(path, file) = change {
                *file = self
                    .sync_data
                    .find_relative_path_index(path)
                    .map(|idx| self.sync_data.files[idx].clone());
            }
        }
        changes
    }

    pub(super) fn replay_restore_changes(&mut self, changes: Vec<RestoreChange>) {
        for change in changes {
            match change {
                RestoreChange::Upsert(path, Some(file)) => self.replay_restore_upsert(&path, file),
                RestoreChange::Upsert(_, None) => {}
                RestoreChange::Remove(path) => {
                    if let Some(index) = self.sync_data.find_relative_path_index(&path) {
                        self.tombstone_file(index);
                    }
                }
                RestoreChange::RemoveDirs(prefixes) => {
                    self.remove_dir_prefixes(&prefixes, None);
                }
            }
        }
    }

    fn replay_restore_upsert(&mut self, path: &str, source: FileItem) {
        let current = self.sync_data.find_relative_path_index(path);
        if source.is_deleted() {
            if let Some(index) = current {
                self.tombstone_file(index);
            }
        } else if let Some(index) = current {
            self.untombstone_file(index);
            let target = &mut self.sync_data.files[index];
            target.update_metadata(&self.cache_budget, None, Some(source.size));
            target.modified = source.modified;
            target.set_binary(source.is_binary());
            if self.enable_content_indexing
                && index < self.sync_data.indexable_count
                && source.size > crate::constants::MAX_INDEXABLE_FILE_SIZE as u64
            {
                // This slot's bigrams cover only the prefix after the file grew.
                self.restored_unindexed.push(index);
            }
        } else {
            if self.sync_data.ensure_capacity(1, 0) {
                self.cache_budget.reset();
            }
            self.append_session_file(source, path);
        }
    }
}
