//! Rill updating itself with Velopack: downloads in the background, installs on restart.

use std::sync::Arc;

use velopack::sources::{FileSource, HttpSource, UpdateSource};
use velopack::{UpdateCheck, UpdateInfo, UpdateManager, VelopackApp};

/// Where releases are published.
pub const FEED: &str = "https://rill.mobasity.com/updates";

/// Runs Velopack's install and update hooks; must be the first thing `main` does.
pub fn hooks() {
    VelopackApp::build().run();
}

/// The feed to check: `RILL_UPDATE_SOURCE` (a folder or an address, for testing) or `FEED`.
fn source() -> Box<dyn UpdateSource> {
    match std::env::var("RILL_UPDATE_SOURCE") {
        Ok(s) if s.starts_with("http://") || s.starts_with("https://") => {
            Box::new(HttpSource::new(&s))
        }
        Ok(s) if !s.trim().is_empty() => Box::new(FileSource::new(s.trim())),
        _ => Box::new(HttpSource::new(FEED)),
    }
}

/// A downloaded update, ready to install.
#[derive(Clone)]
pub struct Ready {
    pub version: String,
    manager: Arc<UpdateManager>,
    info: Arc<UpdateInfo>,
}

impl Ready {
    /// Closes Rill, installs the update and starts the new version.
    pub fn restart(&self) -> Result<(), String> {
        self.manager
            .apply_updates_and_restart(&self.info.TargetFullRelease)
            .map_err(|e| format!("Updates: couldn't start the installer ({e}). Close Rill and open it again to install."))
    }
}

/// What a check found.
pub enum Checked {
    /// A development run, not an installed Rill: nothing to update.
    NotInstalled,
    UpToDate,
    Ready(Ready),
}

/// Checks the feed and downloads any newer version. Blocking: run it off the UI thread.
pub fn check() -> Result<Checked, String> {
    let manager = match UpdateManager::new_boxed(source(), None, None) {
        Ok(m) => m,
        // Velopack can't find its install: this copy wasn't installed with Setup.
        Err(_) => return Ok(Checked::NotInstalled),
    };
    // Downloaded last time but not installed yet.
    if let Some(pending) = manager.get_update_pending_restart() {
        tracing::info!(version = %pending.Version, "update already downloaded");
    }
    let info = match manager.check_for_updates() {
        Ok(UpdateCheck::UpdateAvailable(info)) => info,
        Ok(UpdateCheck::NoUpdateAvailable | UpdateCheck::RemoteIsEmpty) => {
            return Ok(Checked::UpToDate);
        }
        Err(e) => {
            return Err(format!(
                "Updates: couldn't check {} ({e}). Rill tries again later.",
                feed_name()
            ));
        }
    };
    manager.download_updates(&info, None).map_err(|e| {
        format!(
            "Updates: the download of Rill {} failed ({e}). Rill tries again later.",
            info.TargetFullRelease.Version
        )
    })?;
    Ok(Checked::Ready(Ready {
        version: info.TargetFullRelease.Version.clone(),
        manager: Arc::new(manager),
        info: Arc::new(*info),
    }))
}

/// The feed's name for messages.
fn feed_name() -> String {
    std::env::var("RILL_UPDATE_SOURCE")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| FEED.to_string())
}
