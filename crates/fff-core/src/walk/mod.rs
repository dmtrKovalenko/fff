//! Filesystem traversal backend. Selects one implementation at compile time:
//! - `zlob`: zlob's native parallel walker (requires the Zig toolchain).
//! - `ripgrep`: the `ignore` crate (ripgrep's walker), used by default.
//!
//! Both expose [`walk_collect_files`] with identical semantics so the rest of
//! the crate stays backend-agnostic.

use crate::types::FileItem;
use std::path::Path;

#[cfg(feature = "zlob")]
mod zlob;
#[cfg(feature = "zlob")]
pub(crate) use zlob::walk_collect_files;

#[cfg(all(not(feature = "zlob"), feature = "ripgrep"))]
mod ripgrep;
#[cfg(all(not(feature = "zlob"), feature = "ripgrep"))]
pub(crate) use ripgrep::walk_collect_files;

pub(crate) struct WalkOutput {
    pub(crate) pairs: Vec<(FileItem, String)>,
    /// Every non-ignored directory the walk visited, relative, ending with /
    pub(crate) dirs: Vec<String>,
    pub(crate) ignore_rules: Option<WalkIgnoreRules>,
}

pub(crate) struct WalkIgnoreRules {
    #[cfg(feature = "zlob")]
    inner: ::zlob::walk::WalkerOutcomeRules,
    #[cfg(not(feature = "zlob"))]
    _never: std::convert::Infallible,
}

// SAFETY: the underlying storage is immutable, heap-owned, and thread-safe to
// read from concurrently (mirrors zlob's `IgnoreRules: Send + Sync`).
unsafe impl Send for WalkIgnoreRules {}
unsafe impl Sync for WalkIgnoreRules {}

impl std::fmt::Debug for WalkIgnoreRules {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("WalkIgnoreRules")
    }
}

// In ripgrep builds `WalkIgnoreRules` is never constructed (the `_never`
// field is uninhabited), so its methods are legitimately dead there.
#[cfg_attr(not(feature = "zlob"), allow(dead_code))]
impl WalkIgnoreRules {
    /// Returns `true` if the provided path is ignored by the collected rule set
    ///
    /// `relative_path` has to be relative to the walker's provided base path
    pub(crate) fn is_ignored(&self, relative_path: &Path) -> bool {
        #[cfg(feature = "zlob")]
        {
            self.inner
                .rules()
                .is_some_and(|rules| rules.is_ignored(relative_path))
        }
        #[cfg(not(feature = "zlob"))]
        {
            let _ = relative_path;
            match self._never {}
        }
    }

    // The old `is_ignored_untrusted` variant was folded away when zlob's
    // ignore matcher moved to full ancestor enumeration — trailing-slash
    // sniffing on the input is now sufficient for external queries.
}

#[cfg(test)]
mod tests {
    use super::walk_collect_files;
    use std::fs;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // Backend-agnostic parity check: both the zlob and ripgrep walkers must
    // respect .gitignore, skip hidden files in a git repo, and surface the
    // expected file set with a correct synced count.
    #[test]
    fn collects_files_respecting_gitignore() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir(root.join(".git")).unwrap();
        fs::create_dir(root.join("src")).unwrap();
        fs::create_dir(root.join("target")).unwrap();
        fs::write(root.join(".gitignore"), "target/\n*.log\n").unwrap();
        fs::write(root.join("Cargo.toml"), "x").unwrap();
        fs::write(root.join("debug.log"), "").unwrap();
        fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();
        fs::write(root.join("target/out.bin"), "bin").unwrap();

        let counter = Arc::new(AtomicUsize::new(0));
        let out = walk_collect_files(root, true, false, 1, &counter).unwrap();

        let mut names: Vec<String> = out.pairs.into_iter().map(|(_, rel)| rel).collect();
        names.sort();

        assert!(names.contains(&"Cargo.toml".to_string()));
        assert!(names.iter().any(|n| n.ends_with("main.rs")));
        // target/ and *.log are gitignored; .git/ is skipped.
        assert!(!names.iter().any(|n| n.contains("target")));
        assert!(!names.iter().any(|n| n.ends_with(".log")));
        assert!(!names.iter().any(|n| n.contains(".git/")));
        assert_eq!(counter.load(Ordering::Relaxed), names.len());
    }

    // Non-git roots prune known non-code directories (node_modules).
    #[test]
    fn prunes_non_code_dirs_for_non_git_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir(root.join("node_modules")).unwrap();
        fs::write(root.join("node_modules/lib.js"), "x").unwrap();
        fs::write(root.join("index.js"), "x").unwrap();

        let counter = Arc::new(AtomicUsize::new(0));
        let out = walk_collect_files(root, false, false, 1, &counter).unwrap();
        let names: Vec<String> = out.pairs.into_iter().map(|(_, rel)| rel).collect();

        assert!(names.iter().any(|n| n.ends_with("index.js")));
        assert!(!names.iter().any(|n| n.contains("node_modules")));
    }

    // Only the zlob backend surfaces reusable ignore rules; they must match
    // the same tree the walk respected.
    #[cfg(feature = "zlob")]
    #[test]
    fn surfaces_reusable_ignore_rules() {
        use std::path::Path;

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir(root.join(".git")).unwrap();
        fs::write(root.join(".gitignore"), "target/\n*.log\n").unwrap();
        fs::write(root.join("Cargo.toml"), "x").unwrap();

        let counter = Arc::new(AtomicUsize::new(0));
        let out = walk_collect_files(root, true, false, 1, &counter).unwrap();

        let rules = out.ignore_rules.expect("zlob surfaces ignore rules");
        assert!(rules.is_ignored(Path::new("target/")));
        assert!(rules.is_ignored(Path::new("debug.log")));
        assert!(!rules.is_ignored(Path::new("Cargo.toml")));
    }

    // Both backends must honor git's core.excludesFile, not just repo-local
    // .gitignore/.ignore (#874).
    #[test]
    fn honors_git_excludes_file() {
        use std::process::Command;

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join(".worktrees/wt1")).unwrap();
        fs::write(root.join("src/hit.ts"), "x").unwrap();
        fs::write(root.join(".worktrees/wt1/hit.ts"), "x").unwrap();

        let excludes = root.join("global_ignore");
        fs::write(&excludes, "# comment\n\n.worktrees/\n").unwrap();
        let git = |args: &[&std::ffi::OsStr]| {
            assert!(
                Command::new("git")
                    .args(args)
                    .current_dir(root)
                    .status()
                    .unwrap()
                    .success()
            );
        };
        git(&["init".as_ref(), "-q".as_ref()]);
        git(&[
            "config".as_ref(),
            "core.excludesFile".as_ref(),
            excludes.as_ref(),
        ]);

        let counter = Arc::new(AtomicUsize::new(0));
        let out = walk_collect_files(root, true, false, 1, &counter).unwrap();
        let names: Vec<String> = out.pairs.iter().map(|(_, rel)| rel.clone()).collect();

        assert!(names.iter().any(|n| n.ends_with("src/hit.ts")), "{names:?}");
        assert!(
            !names.iter().any(|n| n.contains(".worktrees")),
            "core.excludesFile not honored: {names:?}"
        );

        // The watcher reuses these rules, so excluded dirs must stay excluded
        // on incremental updates too.
        #[cfg(feature = "zlob")]
        {
            let rules = out.ignore_rules.expect("zlob surfaces ignore rules");
            assert!(rules.is_ignored(std::path::Path::new(".worktrees/")));
            assert!(!rules.is_ignored(std::path::Path::new("src/hit.ts")));
        }
    }
}
