//! Per-user base directories: XDG on Unix, the profile folders on Windows.

use std::path::PathBuf;

fn env_dir(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
}

pub fn home_dir() -> Option<PathBuf> {
    env_dir(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
}

pub fn config_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        return env_dir("APPDATA");
    }
    env_dir("XDG_CONFIG_HOME").or_else(|| home_dir().map(|h| h.join(".config")))
}

pub fn data_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        return env_dir("APPDATA");
    }
    env_dir("XDG_DATA_HOME").or_else(|| home_dir().map(|h| h.join(".local").join("share")))
}

pub fn cache_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        return env_dir("LOCALAPPDATA");
    }
    env_dir("XDG_CACHE_HOME").or_else(|| home_dir().map(|h| h.join(".cache")))
}
