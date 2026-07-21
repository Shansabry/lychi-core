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

pub fn clipboard_images_dir() -> PathBuf {
    data_dir().join("clipboard-images")
}
