//! Rill's OS integrations, kept small and behind plain functions.

pub mod credentials;
pub mod known;
pub mod settings;
pub mod window;

use std::path::PathBuf;

/// Where Rill keeps settings, logs and caches.
///
/// `RILL_DATA_DIR` overrides it (test scripts point it into `.dev/`). Otherwise debug builds use
/// the repository's `.dev/` folder, so a local run touches nothing outside it.
pub fn data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("RILL_DATA_DIR").filter(|d| !d.is_empty()) {
        return PathBuf::from(dir);
    }
    if cfg!(debug_assertions) {
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let repo = manifest.ancestors().nth(2).unwrap_or(manifest);
        return repo.join(".dev");
    }
    platform_data_dir().join("Rill")
}

/// The folder for log files.
pub fn log_dir() -> PathBuf {
    data_dir().join("logs")
}

/// The service name Rill's tokens are stored under in the OS credential store.
pub fn credential_service() -> &'static str {
    if cfg!(debug_assertions) {
        "rill-dev"
    } else {
        "rill"
    }
}

#[cfg(windows)]
fn platform_data_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join("AppData").join("Local"))
}

#[cfg(target_os = "macos")]
fn platform_data_dir() -> PathBuf {
    home().join("Library").join("Application Support")
}

#[cfg(all(unix, not(target_os = "macos")))]
fn platform_data_dir() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home().join(".local").join("share"))
}

fn home() -> PathBuf {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_builds_stay_in_the_repository() {
        let dir = data_dir();
        assert!(dir.ends_with(".dev"));
        assert!(log_dir().starts_with(&dir));
        assert_eq!(credential_service(), "rill-dev");
    }
}
