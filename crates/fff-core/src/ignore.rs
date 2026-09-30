use std::path::{Path, PathBuf};

/// Directories excluded when walking a non-git root. Entries are `cfg`-gated
/// so a single iteration covers standard + platform-specific overrides.
pub(crate) const IGNORED_DIRS: &[&str] = &[
    // various dev tools that can be meet in the developer app
    "node_modules",
    "__pycache__",
    "venv",
    ".venv",
    "target/debug",
    "target/release",
    "target/rust-analyzer",
    "target/criterion",
    // Language package caches in non-git roots.
    "go/pkg/mod",
    ".cargo/registry",
    ".rustup/toolchains",
    ".gradle/caches",
    ".m2/repository",
    ".npm/_cacache",
    ".pub-cache",
    #[cfg(not(target_os = "windows"))]
    ".local/state", // this contains tons of logs which generate too much watcher noise
    #[cfg(target_os = "macos")]
    "Library/Application Support",
    #[cfg(target_os = "macos")]
    "Library/Caches",
    #[cfg(target_os = "macos")]
    "Library/Containers", // sandboxed apps data
    #[cfg(target_os = "macos")]
    "Library/Group Containers", // random application data and networking
    #[cfg(target_os = "macos")]
    "Library/pnpm",
    #[cfg(target_os = "macos")]
    "Library/Metadata",
    #[cfg(target_os = "macos")]
    "Library/Developer/CoreSimulator",
    #[cfg(target_os = "macos")]
    "Library/Android",
    #[cfg(target_os = "macos")]
    "Library/Logs",
    #[cfg(target_os = "macos")]
    "Library/Daemon Containers",
    #[cfg(target_os = "macos")]
    "Library/Trial",
    #[cfg(target_os = "macos")]
    "Library/Preferences",
    #[cfg(target_os = "macos")]
    "Library/Messages",
    #[cfg(target_os = "macos")]
    "Library/IdentityServices",
    #[cfg(target_os = "windows")]
    "bin/Debug",
    #[cfg(target_os = "windows")]
    "bin/Release",
    #[cfg(target_os = "windows")]
    "Program Files",
    #[cfg(target_os = "windows")]
    "Program Files (x86)",
    #[cfg(target_os = "windows")]
    "AppData/Local",
    #[cfg(target_os = "windows")]
    "AppData/Roaming",
];

/// Resolves git's `core.excludesFile` through the repo's own config chain, so
/// repo-local, global and system settings all win in git's precedence order.
pub(crate) fn git_excludes_file(base_path: &Path) -> Option<PathBuf> {
    let config = git2::Repository::discover(base_path)
        .and_then(|repo| repo.config())
        .or_else(|_| git2::Config::open_default())
        .ok()?;

    // `get_path` expands a leading `~` for us; a configured-but-absent file is
    // legal in git and simply matches nothing.
    let path = config.get_path("core.excludesFile").ok()?;
    path.is_file().then_some(path)
}

/// `core.excludesFile` contents as gitignore patterns, comments stripped.
#[cfg(feature = "zlob")]
pub(crate) fn git_excludes_patterns(base_path: &Path) -> Vec<String> {
    let Some(path) = git_excludes_file(base_path) else {
        return Vec::new();
    };

    match std::fs::read_to_string(&path) {
        Ok(contents) => contents
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .map(str::to_owned)
            .collect(),
        Err(e) => {
            tracing::warn!(?e, ?path, "core.excludesFile unreadable; not applied");
            Vec::new()
        }
    }
}

#[cfg(all(not(feature = "zlob"), feature = "ripgrep"))]
pub(crate) fn non_git_repo_overrides(base_path: &Path) -> Option<ignore::overrides::Override> {
    use ignore::overrides::OverrideBuilder;

    let mut builder = OverrideBuilder::new(base_path);
    for dir in IGNORED_DIRS {
        let pattern = format!("!**/{dir}/");
        if let Err(e) = builder.add(&pattern) {
            tracing::warn!("failed to add ignore pattern {pattern}: {e}");
        }
    }

    builder.build().ok()
}

pub(crate) fn is_non_code_directory(path: &Path) -> bool {
    let path_str = path.as_os_str().to_str().unwrap_or("");
    IGNORED_DIRS.iter().any(|&dir| {
        // Entries are gitignore patterns for the walkers; here they are matched
        // as substrings, so a leading `*` wildcard has to come off first.
        let dir = dir.strip_prefix('*').unwrap_or(dir);

        #[cfg(target_os = "windows")]
        let dir = dir.replace('/', std::path::MAIN_SEPARATOR_STR);
        #[cfg(target_os = "windows")]
        return path_str.contains(dir.as_str());

        #[cfg(not(target_os = "windows"))]
        path_str.contains(dir)
    })
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn home_machine_state_is_excluded_but_source_trees_are_not() {
        // Representative machine state from a home index.
        for rel in [
            "Library/pnpm/store/v3/files/00/abcdef",
            "Library/Preferences/com.apple.finder.plist",
            "Library/Messages/prewarm.db-shm",
            "Library/IdentityServices/TetraDB-identityservicesd.db-wal",
            "Library/Developer/CoreSimulator/Devices/X/data/f",
            "go/pkg/mod/github.com/x/y@v1/main.go",
            ".cargo/registry/src/index.crates.io-1/serde-1.0/src/lib.rs",
            "Library/Android/sdk/platforms/android-34/data/x",
            ".local/state/nvim/fff+123+456.log",
        ] {
            assert!(
                is_non_code_directory(Path::new(rel)),
                "{rel} must not reach the index"
            );
        }

        // Source trees under $HOME stay searchable.
        for rel in [
            "dev/chromium/third_party/blink/renderer/core/dom/node.cc",
            "dev/fff/crates/fff-core/src/lib.rs",
            "Documents/notes/todo.md",
            "dev/myproj/pkg/mod/thing.go",
        ] {
            assert!(
                !is_non_code_directory(Path::new(rel)),
                "{rel} must stay searchable"
            );
        }
    }
}
