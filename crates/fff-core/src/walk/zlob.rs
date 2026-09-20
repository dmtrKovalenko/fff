use crate::file_picker::is_known_binary_extension_basename;
use crate::ignore::IGNORED_DIRS;
use crate::types::FileItem;
use crate::walk::{WalkIgnoreRules, WalkOutput};
use parking_lot::Mutex;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use zlob::walk::{WalkBuilder, WalkFlags, WalkMetadata, WalkState};

const PROGRESS_STEP: usize = 13;

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

    // zlob reads nested .gitignore/.ignore itself but never git's
    // core.excludesFile, so layer that in as root-level extra rules (#874).
    let global_excludes = if is_git_repo {
        crate::ignore::git_excludes_patterns(base_path)
    } else {
        Vec::new()
    };
    let extra_ignore: Vec<&str> = if is_git_repo {
        global_excludes.iter().map(String::as_str).collect()
    } else {
        IGNORED_DIRS.to_vec()
    };

    if !extra_ignore.is_empty()
        && let Err(e) = builder.extra_ignore(&extra_ignore)
    {
        // Interior NUL in one of the extra_ignore patterns would fail
        // here — treat as if no extras were supplied rather than
        // aborting the whole walk.
        tracing::warn!(?e, "zlob extra_ignore rejected; walking without it");
    }

    // Single lock for both collections: every entry is either a file or a
    // dir, so this keeps one mutex acquisition per entry.
    let collected = Mutex::new((Vec::new(), Vec::new()));

    let outcome = match builder.run(|entry| {
        if !entry.is_file() {
            // unlike ripgrep walker zlob doesnt show .git files
            if entry.is_dir() {
                let relative_path = entry.relative_path_lossy();
                if !relative_path.is_empty() {
                    let mut relative_path = relative_path.into_owned();
                    relative_path.push('/');
                    collected.lock().1.push(relative_path);
                }
            }

            return WalkState::Continue;
        }

        // `basename()` returns `&str` for files only.
        let basename = entry.basename().unwrap_or("");
        let is_binary = is_known_binary_extension_basename(basename);

        let size = entry.size().unwrap_or(0);
        // zlob reports mtime in ns since the Unix epoch; FileItem wants secs.
        let modified = entry
            .modified_ns()
            .map(|ns| (ns / 1_000_000_000).max(0) as u64)
            .unwrap_or(0);

        // Lossy pair: the offset must index the decoded string, not the raw
        // bytes, or it lands inside a U+FFFD on invalid-UTF-8 names (#799).
        let basename_offset = entry.basename_offset_in_relative_lossy() as u16;
        // zlob emits '/'-separated relative paths, which is fff's canonical
        // internal form on every platform — store them verbatim.
        let relative_path = entry.relative_path_lossy().into_owned();
        let item = FileItem::new_raw(basename_offset, size, modified, None, is_binary);

        let mut guard = collected.lock();
        guard.0.push((item, relative_path));
        let n = guard.0.len();
        drop(guard);

        if n % PROGRESS_STEP == 0 {
            synced_files_count.store(n, Ordering::Relaxed);
        }

        WalkState::Continue
    }) {
        Ok(outcome) => outcome,
        Err(e) => {
            // Preserve whatever we collected before the failure so the caller
            // can still surface a partial index instead of nothing.
            tracing::error!(?e, "zlob walk failed");
            return Err(crate::Error::WalkFailed(format!("{e:?}")));
        }
    };

    let (pairs, dirs) = collected.into_inner();
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
