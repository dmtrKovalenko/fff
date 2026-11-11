use std::path::StripPrefixError;

#[derive(thiserror::Error, Debug)]
#[non_exhaustive]
pub enum Error {
    #[error("Thread panicked")]
    ThreadPanic,
    #[error("Invalid path {0}")]
    InvalidPath(std::path::PathBuf),
    #[error(
        "Can not run certain FFF features in a file system root or home directories. Consider smaller per-project directories."
    )]
    FilesystemRoot(std::path::PathBuf),
    #[error("File picker not initialized")]
    FilePickerMissing,
    #[error("Index is busy with post-scan processing, retry later")]
    IndexBusy,
    #[error("Layer file {path}: {source}")]
    LayerFile {
        path: std::path::PathBuf,
        source: std::io::Error,
    },
    #[error("Invalid layer file: {0}")]
    LayerFormat(&'static str),
    #[error("Failed to acquire lock for frecency")]
    AcquireFrecencyLock,
    #[error("Failed to acquire lock for items by provider")]
    AcquireItemLock,
    #[error("Failed to acquire lock for path cache")]
    AcquirePathCacheLock,
    #[error("Failed to create directory: {0}")]
    CreateDir(#[from] std::io::Error),
    #[error("Failed to remove database directory {path}: {source}")]
    RemoveDbDir {
        path: std::path::PathBuf,
        source: std::io::Error,
    },
    #[error("Something is wrong with the local db instance: {0}")]
    GenericDbError(#[from] heed::Error),
    #[error("Failed to open {db} database env: {source}")]
    EnvOpen {
        db: &'static str,
        #[source]
        source: heed::Error,
    },
    #[error(
        "LMDB env at {path} is already open as the '{open_as}' database with different options; requested by '{requested_as}'. Use a distinct path per database."
    )]
    EnvSpecMismatch {
        path: std::path::PathBuf,
        open_as: &'static str,
        requested_as: &'static str,
    },
    #[error(
        "The {db} database at {path} is still used by {holders} other tracker(s) in this process"
    )]
    DbInUse {
        db: &'static str,
        path: std::path::PathBuf,
        holders: usize,
    },
    #[error("Failed to create {db} database: {source}")]
    DbCreate {
        db: &'static str,
        #[source]
        source: heed::Error,
    },
    #[error("Failed to open {db} database: {source}")]
    DbOpen {
        db: &'static str,
        #[source]
        source: heed::Error,
    },
    #[error("Failed to clear stale readers for {db} database: {source}")]
    DbClearStaleReaders {
        db: &'static str,
        #[source]
        source: heed::Error,
    },

    #[error("Failed to start read transaction for {db} database: {source}")]
    DbStartReadTxn {
        db: &'static str,
        #[source]
        source: heed::Error,
    },
    #[error("Failed to start write transaction for {db} database: {source}")]
    DbStartWriteTxn {
        db: &'static str,
        #[source]
        source: heed::Error,
    },
    #[error("Failed to read from {db} database: {source}")]
    DbRead {
        db: &'static str,
        #[source]
        source: heed::Error,
    },
    #[error("Failed to write to {db} database: {source}")]
    DbWrite {
        db: &'static str,
        #[source]
        source: heed::Error,
    },
    #[error("Failed to commit write transaction to {db} database: {source}")]
    DbCommit {
        db: &'static str,
        #[source]
        source: heed::Error,
    },
    #[error("Failed to start file system watcher: {0}")]
    FileSystemWatch(#[from] notify::Error),

    #[error("Expected a path to be child of another path: {0}")]
    StripPrefixError(#[from] StripPrefixError),

    #[error("libgit2 error occurred: {0}")]
    Git(#[from] git2::Error),

    #[error("Filesystem walk failed: {0}")]
    WalkFailed(String),

    #[error("Invalid glob pattern '{pattern}': {reason}")]
    InvalidGlobPattern { pattern: String, reason: String },

    #[error("File system watching is disabled for this picker")]
    WatcherDisabled,

    #[error("File system watcher is not ready")]
    WatcherNotReady,

    #[error("Indexed base path changed while creating the watch subscription")]
    WatchBaseChanged,

    #[error("Failed to start watch callback dispatcher: {0}")]
    WatchDispatcherStart(#[source] std::io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
