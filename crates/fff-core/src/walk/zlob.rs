use crate::file_picker::{
    is_ambiguous_binary_name, is_known_binary_extension_basename, starts_with_binary_content,
};
use crate::ignore::IGNORED_DIRS;
use crate::types::FileItem;
use crate::walk::{WalkIgnoreRules, WalkOutput};
use parking_lot::Mutex;
use std::borrow::Cow;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::SystemTime;
use zlob::walk::{WalkBuilder, WalkFlags, WalkMetadata, WalkState};

const PROGRESS_STEP: usize = 13;

// Keep adjacent workers' locks and vector headers on separate cache lines.
#[repr(align(128))]
#[derive(Default)]
struct WorkerFiles {
    pairs: Vec<(FileItem, String)>,
    dirs: Vec<String>,
}

#[tracing::instrument(skip_all, name = "zlob walker", level = "info")]
pub(crate) fn walk_collect_files(
    base_path: &Path,
    is_git_repo: bool,
    follow_symlinks: bool,
    threads: usize,
    synced_files_count: &Arc<AtomicUsize>,
) -> crate::Result<WalkOutput> {
    // gitignore on; skip hidden on non-git roots (so `~/` doesn't recurse into
    // ~/.cache, ~/.config, etc.); optionally follow symlinks.
    let mut flags = WalkFlags::GITIGNORE;
    if !is_git_repo {
        flags |= WalkFlags::SKIP_HIDDEN;
    }
    if follow_symlinks {
        flags |= WalkFlags::FOLLOW_SYMLINKS;
    }

    let mut builder = WalkBuilder::new(base_path)
        .map_err(|e| crate::Error::WalkFailed(format!("WalkBuilder::new: {e:?}")))?;
    builder
        .options(flags)
        .threads(threads)
        // Bulk-fetch the only metadata FileItem needs; zlob never stats more.
        .metadata(WalkMetadata::SIZE | WalkMetadata::MTIME);

    if !is_git_repo
        && !IGNORED_DIRS.is_empty()
        && let Err(e) = builder.extra_ignore(IGNORED_DIRS)
    {
        // Interior NUL in one of the extra_ignore patterns would fail
        // here — treat as if no extras were supplied rather than
        // aborting the whole walk.
        tracing::warn!(?e, "zlob extra_ignore rejected; walking without it");
    }

    let collected: Vec<Mutex<WorkerFiles>> = (0..builder.max_workers())
        .map(|_| Mutex::default())
        .collect();
    synced_files_count.store(0, Ordering::Relaxed);

    let outcome = match builder.run(|entry| {
        let collected = &collected[entry.worker_id()];
        // Followed links keep the Symlink kind and lstat metadata, so stat
        // the target to index the ones resolving to files (#912).
        let target = if entry.is_file() {
            None
        } else if follow_symlinks && entry.is_symlink() {
            match std::fs::metadata(entry.path()) {
                Ok(meta) if meta.is_file() => Some(meta),
                _ => return WalkState::Continue,
            }
        } else {
            // unlike ripgrep walker zlob doesnt show .git files
            if entry.is_dir() {
                let relative_path = entry.relative_path_lossy();
                if !relative_path.is_empty() {
                    let mut relative_path = match relative_path {
                        Cow::Borrowed(path) => {
                            let mut relative_path = String::with_capacity(path.len() + 1);
                            relative_path.push_str(path);
                            relative_path
                        }
                        Cow::Owned(path) => path,
                    };
                    relative_path.push('/');
                    collected.lock().dirs.push(relative_path);
                }
            }

            return WalkState::Continue;
        };

        // Lossy pair: the offset must index the decoded string, not the raw
        // bytes, or it lands inside a U+FFFD on invalid-UTF-8 names (#799).
        let basename_offset = entry.basename_offset_in_relative_lossy();
        // zlob emits '/'-separated relative paths, which is fff's canonical
        // internal form on every platform — store them verbatim.
        let relative_path = entry.relative_path_lossy().into_owned();

        // `basename()` returns `&str` for files only.
        let basename = match target {
            None => entry.basename().unwrap_or(""),
            Some(_) => &relative_path[basename_offset..],
        };
        if basename == ".git" {
            return WalkState::Continue;
        }
        let is_binary = is_known_binary_extension_basename(basename)
            && (!is_ambiguous_binary_name(basename) || starts_with_binary_content(entry.path()));

        let (size, modified) = match target {
            Some(meta) => (
                meta.len(),
                meta.modified()
                    .ok()
                    .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
                    .map_or(0, |d| d.as_secs()),
            ),
            // zlob reports mtime in ns since the Unix epoch; FileItem wants secs.
            None => (
                entry.size().unwrap_or(0),
                entry
                    .modified_ns()
                    .map(|ns| (ns / 1_000_000_000).max(0) as u64)
                    .unwrap_or(0),
            ),
        };

        let mut item = FileItem::new_raw(basename_offset as u16, size, modified, None, is_binary);
        item.mark_definition_language(basename);

        let mut guard = collected.lock();
        guard.pairs.push((item, relative_path));
        let n = guard.pairs.len();
        drop(guard);

        if n.is_multiple_of(PROGRESS_STEP) {
            synced_files_count.fetch_add(PROGRESS_STEP, Ordering::Relaxed);
        }

        WalkState::Continue
    }) {
        Ok(outcome) => outcome,
        Err(e) => {
            tracing::error!(?e, "zlob walk failed");
            return Err(crate::Error::WalkFailed(format!("{e:?}")));
        }
    };

    let collected: Vec<_> = collected.into_iter().map(Mutex::into_inner).collect();
    let mut pairs = Vec::with_capacity(collected.iter().map(|shard| shard.pairs.len()).sum());
    let mut dirs = Vec::with_capacity(collected.iter().map(|shard| shard.dirs.len()).sum());
    for mut shard in collected {
        pairs.append(&mut shard.pairs);
        dirs.append(&mut shard.dirs);
    }
    // Always report the exact final total regardless of the last step.
    synced_files_count.store(pairs.len(), Ordering::Relaxed);

    // Retain the ignore rules only when the walk actually gathered some
    // (git roots with .gitignore/.ignore). Otherwise callers fall back.
    let ignore_rules = outcome
        .rules()
        .is_some()
        .then(|| WalkIgnoreRules { inner: outcome });

    Ok(WalkOutput {
        pairs,
        dirs,
        ignore_rules,
    })
}
