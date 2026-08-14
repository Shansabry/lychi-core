use directories::ProjectDirs;
use std::path::PathBuf;

const QUALIFIER: &str = "app";
const ORGANIZATION: &str = "lychi";
const APPLICATION: &str = "lychi";

/// Returns XDG project directories. Panics if $HOME is unset.
pub fn project_dirs() -> ProjectDirs {
    ProjectDirs::from(QUALIFIER, ORGANIZATION, APPLICATION)
        .expect("Failed to determine XDG project directories — is $HOME set?")
}

pub fn config_dir() -> PathBuf {
    project_dirs().config_dir().to_path_buf()
}

pub fn data_dir() -> PathBuf {
    project_dirs().data_dir().to_path_buf()
}

pub fn config_file() -> PathBuf {
    config_dir().join("config.toml")
}

/// Directory of user Script Commands. A file dropped here becomes a named
/// launcher command (keyword = filename stem). Lives alongside `config.toml`.
pub fn scripts_dir() -> PathBuf {
    config_dir().join("scripts")
}

pub fn db_file() -> PathBuf {
    data_dir().join("lychi.redb")
}

/// The frecency database. Derived, device-local ranking data lives in its OWN
/// redb file, separate from the user-data `lychi.redb`: it keeps the main DB to
/// user-authored content and isolates frecency's growth/corruption. redb (not a
/// flat file) because frecency is the hot-path, keyed, multi-process store its
/// engine is built for — see `db::frecency`.
pub fn frecency_db_file() -> PathBuf {
    data_dir().join("frecency.redb")
}

pub fn clipboard_images_dir() -> PathBuf {
    data_dir().join("clipboard-images")
}

/// Running-timer state. Device-local (bound to this session's wall clock), so it
/// lives in a file rather than the user-data database — see `filestore`.
pub fn timers_file() -> PathBuf {
    data_dir().join("timers.json")
}

/// Learned per-model capabilities (vision support, capability-meter estimate).
/// Derived machine-learned data, not user content — a JSONL file, not the DB.
pub fn model_caps_file() -> PathBuf {
    data_dir().join("model-caps.jsonl")
}

/// AI chat transcripts — one `<id>.json` file per conversation. Large, append-y,
/// arguably local; kept out of the user-data DB so deleting a conversation
/// reclaims its disk immediately (unlink) and retention is a directory sweep.
pub fn ai_history_dir() -> PathBuf {
    data_dir().join("ai_history")
}

/// Command history — a JSONL log of past commands (device-local usage record,
/// not portable user content). Newest-last, deduped, capped.
pub fn history_file() -> PathBuf {
    data_dir().join("history.jsonl")
}

/// Clipboard history — a JSONL log (device-local, sensitive; image bytes live in
/// `clipboard_images_dir`, this holds text + path + thumbnail). 0600.
pub fn clipboard_file() -> PathBuf {
    data_dir().join("clipboard.jsonl")
}

/// Where backup archives live. Inside the data dir so a user copying that one
/// directory takes their backups with them, but excluded from the archive
/// itself so backups never nest.
pub fn backups_dir() -> PathBuf {
    data_dir().join("backups")
}

/// Directory where downloaded local-AI model weights (GGUF) are stored. Weights
/// are fetched on first use (not bundled), so this lives in the data dir.
pub fn models_dir() -> PathBuf {
    data_dir().join("models")
}
