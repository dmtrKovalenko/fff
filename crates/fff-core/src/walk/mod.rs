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

#[cfg_attr(not(feature = "zlob"), derive(Clone))]
pub(crate) struct WalkIgnoreRules {
    #[cfg(feature = "zlob")]
    inner: ::zlob::walk::WalkerOutcomeRules,
    #[cfg(feature = "zlob")]
    skip_hidden: bool,
    #[cfg(not(feature = "zlob"))]
    inner: ::ignore::IncrementalIgnore,
}

// SAFETY: the underlying storage is immutable, heap-owned, and thread-safe to
// read from concurrently (mirrors zlob's `IgnoreRules: Send + Sync`).
#[cfg(feature = "zlob")]
unsafe impl Send for WalkIgnoreRules {}
#[cfg(feature = "zlob")]
unsafe impl Sync for WalkIgnoreRules {}

impl std::fmt::Debug for WalkIgnoreRules {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("WalkIgnoreRules")
    }
}

impl WalkIgnoreRules {
    #[cfg(feature = "zlob")]
    pub(crate) fn is_ignored(&self, relative_path: &Path, _is_dir: bool) -> bool {
        if self.skip_hidden
            && relative_path
                .iter()
                .any(|name| name.as_encoded_bytes().starts_with(b"."))
        {
            return true;
        }
        self.inner
            .rules()
            .is_some_and(|rules| rules.is_ignored(relative_path))
    }

    #[cfg(not(feature = "zlob"))]
    pub(crate) fn is_ignored(&mut self, relative_path: &Path, is_dir: bool) -> bool {
        self.inner.matched(relative_path, is_dir).is_ignore()
    }
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
        let out = walk_collect_files(
            root,
            Some(crate::scm::ScmBackend::configured()),
            false,
            1,
            &counter,
            &Default::default(),
            None,
        )
        .unwrap();

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

    #[test]
    fn parallel_walk_preserves_files_directories_and_count() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir(root.join(".git")).unwrap();
        for directory in 0..64 {
            let path = root.join(format!("src{directory}"));
            fs::create_dir(&path).unwrap();
            for file in 0..32 {
                fs::write(path.join(format!("file{file}.rs")), "fn main() {}\n").unwrap();
            }
        }
        let mut results = Vec::new();
        for threads in [1, 4] {
            let counter = Arc::new(AtomicUsize::new(0));
            let out = walk_collect_files(
                root,
                true,
                false,
                threads,
                &counter,
                &Default::default(),
                None,
            )
            .unwrap();
            let mut files: Vec<_> = out
                .pairs
                .into_iter()
                .map(|(item, path)| (path, item.size))
                .collect();
            files.sort();
            let mut directories = out.dirs;
            directories.sort();
            results.push((files, directories, counter.load(Ordering::Relaxed)));
        }
        assert_eq!(results[0], results[1]);
        assert_eq!(results[1].2, 64 * 32);
    }

    #[test]
    fn skips_git_metadata_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir(root.join("module")).unwrap();
        fs::write(root.join(".git"), "gitdir: ../admin\n").unwrap();
        fs::write(
            root.join("module/.git"),
            "gitdir: ../../admin/modules/lib\n",
        )
        .unwrap();
        fs::write(root.join("main.rs"), "fn main() {}\n").unwrap();
        fs::write(root.join("module/lib.rs"), "pub fn lib() {}\n").unwrap();
        let counter = Arc::new(AtomicUsize::new(0));
        let out =
            walk_collect_files(root, true, false, 4, &counter, &Default::default(), None).unwrap();
        let mut paths: Vec<_> = out.pairs.into_iter().map(|(_, path)| path).collect();
        paths.sort();
        assert_eq!(
            (paths, counter.load(Ordering::Relaxed)),
            (vec!["main.rs".to_string(), "module/lib.rs".to_string()], 2)
        );
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
        let out =
            walk_collect_files(root, None, false, 1, &counter, &Default::default(), None).unwrap();
        let names: Vec<String> = out.pairs.into_iter().map(|(_, rel)| rel).collect();

        assert!(names.iter().any(|n| n.ends_with("index.js")));
        assert!(!names.iter().any(|n| n.contains("node_modules")));
    }

    // Stow-style file symlinks pointing outside the root must be indexed with
    // follow_symlinks on, with the target's metadata (#912).
    #[cfg(unix)]
    #[test]
    fn follows_file_symlinks_outside_root() {
        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("dotfiles");
        let root = dir.path().join("config");
        fs::create_dir_all(outside.join("nested")).unwrap();
        fs::create_dir(&root).unwrap();
        fs::write(outside.join("sesh.toml"), "0123456789").unwrap();
        fs::write(outside.join("nested/inner.toml"), "x").unwrap();
        fs::write(root.join("projects.toml"), "x").unwrap();
        std::os::unix::fs::symlink("../dotfiles/sesh.toml", root.join("sesh.toml")).unwrap();
        std::os::unix::fs::symlink("../dotfiles/nested", root.join("linked")).unwrap();
        std::os::unix::fs::symlink("../dotfiles/missing", root.join("broken")).unwrap();

        let counter = Arc::new(AtomicUsize::new(0));
        let out =
            walk_collect_files(&root, None, true, 1, &counter, &Default::default(), None).unwrap();
        let mut names: Vec<String> = out.pairs.iter().map(|(_, rel)| rel.clone()).collect();
        names.sort();

        assert_eq!(names, ["linked/inner.toml", "projects.toml", "sesh.toml"]);
        assert_eq!(counter.load(Ordering::Relaxed), names.len());

        let (item, rel) = out.pairs.iter().find(|(_, r)| r == "sesh.toml").unwrap();
        assert_eq!(item.size, 10);
        assert_eq!(&rel[item.filename_offset_in_relative_path()..], "sesh.toml");

        let out =
            walk_collect_files(&root, None, false, 1, &counter, &Default::default(), None).unwrap();
        let names: Vec<String> = out.pairs.into_iter().map(|(_, rel)| rel).collect();
        assert_eq!(names, ["projects.toml"]);
    }

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
        let out = walk_collect_files(
            root,
            Some(crate::scm::ScmBackend::configured()),
            false,
            1,
            &counter,
            &Default::default(),
            None,
        )
        .unwrap();

        let rules = out.ignore_rules.expect("zlob surfaces ignore rules");
        assert!(rules.is_ignored(Path::new("target/"), true));
        assert!(rules.is_ignored(Path::new("debug.log"), false));
        assert!(!rules.is_ignored(Path::new("Cargo.toml"), false));
    }

    // Both backends must honor `info/exclude`, which linked worktrees read from
    // the common git dir (#982).
    #[test]
    fn honors_git_info_exclude() {
        use std::path::Path;
        use std::process::Command;

        let git = |dir: &Path, args: &[&str]| {
            let status = Command::new("git")
                .args(args)
                .current_dir(dir)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .status()
                .unwrap();
            assert!(status.success(), "git {args:?}");
        };

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join("excluded")).unwrap();
        fs::write(root.join("src/a.txt"), "a").unwrap();
        fs::write(root.join(".gitignore"), "*.log\n").unwrap();
        git(&root, &["init", "-q"]);
        git(&root, &["add", "."]);
        git(&root, &["commit", "-q", "-m", "init"]);
        fs::write(root.join("excluded/b.txt"), "b").unwrap();
        fs::write(root.join(".git/info/exclude"), "# comment\nexcluded/\n").unwrap();

        let worktree = dir.path().join("wt");
        git(
            &root,
            &["worktree", "add", "-q", worktree.to_str().unwrap()],
        );
        fs::create_dir_all(worktree.join("excluded")).unwrap();
        fs::write(worktree.join("excluded/c.txt"), "c").unwrap();

        for base in [&root, &worktree] {
            let counter = Arc::new(AtomicUsize::new(0));
            let out = walk_collect_files(
                base,
                Some(crate::scm::ScmBackend::configured()),
                false,
                1,
                &counter,
                &Default::default(),
                None,
            )
            .unwrap();
            let names: Vec<String> = out.pairs.iter().map(|(_, rel)| rel.clone()).collect();

            assert!(names.iter().any(|n| n == "src/a.txt"), "{names:?}");
            assert!(
                !names.iter().any(|n| n.starts_with("excluded")),
                "{names:?}"
            );

            // The watcher filters incremental updates with these rules.
            #[cfg(feature = "zlob")]
            {
                let rules = out.ignore_rules.expect("zlob surfaces ignore rules");
                assert!(rules.is_ignored(Path::new("excluded/later/new.txt"), false));
                assert!(rules.is_ignored(Path::new("debug.log"), false));
                assert!(!rules.is_ignored(Path::new("src/a.txt"), false));
            }
        }
    }

    // zlob 1.8 reads core.excludesFile below the root .gitignore, as git does (#874).
    #[cfg(feature = "zlob")]
    #[test]
    fn honors_git_excludes_file_with_git_precedence() {
        use std::process::Command;

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join(".worktrees/wt1")).unwrap();
        fs::write(root.join("src/hit.ts"), "x").unwrap();
        fs::write(root.join(".worktrees/wt1/hit.ts"), "x").unwrap();
        fs::write(root.join("drop.tmp"), "x").unwrap();
        fs::write(root.join("keep.tmp"), "x").unwrap();
        fs::write(root.join(".gitignore"), "!keep.tmp\n").unwrap();

        let excludes = dir.path().join("excludes");
        fs::write(&excludes, "# comment\n.worktrees/\n*.tmp\nexcludes\n").unwrap();
        for args in [
            vec!["init", "-q"],
            vec!["config", "core.excludesFile", excludes.to_str().unwrap()],
        ] {
            let status = Command::new("git")
                .args(args)
                .current_dir(root)
                .status()
                .unwrap();
            assert!(status.success());
        }

        let counter = Arc::new(AtomicUsize::new(0));
        let out = walk_collect_files(
            root,
            Some(crate::scm::ScmBackend::configured()),
            false,
            1,
            &counter,
            &Default::default(),
            None,
        )
        .unwrap();
        let names: Vec<String> = out.pairs.iter().map(|(_, rel)| rel.clone()).collect();

        assert!(names.iter().any(|n| n == "src/hit.ts"), "{names:?}");
        assert!(names.iter().any(|n| n == "keep.tmp"), "{names:?}");
        assert!(!names.iter().any(|n| n == "drop.tmp"), "{names:?}");
        assert!(!names.iter().any(|n| n.contains(".worktrees")), "{names:?}");

        // The watcher filters incremental updates with these rules.
        let rules = out.ignore_rules.expect("zlob surfaces ignore rules");
        assert!(rules.is_ignored(std::path::Path::new(".worktrees/wt1/new.ts"), false));
        assert!(!rules.is_ignored(std::path::Path::new("src/hit.ts"), false));
    }

    // disable_git must also turn off the git exclude files, not only .gitignore.
    #[cfg(feature = "zlob")]
    #[test]
    fn disable_git_skips_every_git_ignore_source() {
        use crate::file_picker::FilePickerIgnoreOptions;
        use std::process::Command;

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        fs::create_dir_all(&root).unwrap();
        for name in ["from-gitignore", "from-exclude", "from-global", "from-dot"] {
            fs::write(root.join(name), "x").unwrap();
        }
        fs::write(root.join(".gitignore"), "from-gitignore\n").unwrap();
        fs::write(root.join(".ignore"), "from-dot\n").unwrap();
        let excludes = dir.path().join("excludes");
        fs::write(&excludes, "from-global\n").unwrap();

        for args in [
            vec!["init", "-q"],
            vec!["config", "core.excludesFile", excludes.to_str().unwrap()],
        ] {
            let status = Command::new("git")
                .args(args)
                .current_dir(&root)
                .status()
                .unwrap();
            assert!(status.success());
        }
        fs::write(root.join(".git/info/exclude"), "from-exclude\n").unwrap();

        let walk = |disable_git| -> Vec<String> {
            let options = FilePickerIgnoreOptions {
                disable_git,
                ..Default::default()
            };
            let counter = Arc::new(AtomicUsize::new(0));
            walk_collect_files(
                &root,
                Some(crate::scm::ScmBackend::configured()),
                false,
                1,
                &counter,
                &options,
                None,
            )
            .unwrap()
            .pairs
            .into_iter()
            .map(|(_, rel)| rel)
            .collect()
        };

        let enabled = walk(false);
        for name in ["from-gitignore", "from-exclude", "from-global", "from-dot"] {
            assert!(!enabled.iter().any(|n| n == name), "{name}: {enabled:?}");
        }

        let disabled = walk(true);
        for name in ["from-gitignore", "from-exclude", "from-global"] {
            assert!(disabled.iter().any(|n| n == name), "{name}: {disabled:?}");
        }
        assert!(!disabled.iter().any(|n| n == "from-dot"), "{disabled:?}");
        assert!(
            !disabled.iter().any(|n| n.starts_with(".git/")),
            "{disabled:?}"
        );
    }
}
