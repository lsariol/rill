//! The window's last size and position, kept apart from the hand-edited `settings.json`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::data_dir;

/// Where and how big the window was, in logical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Placement {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub maximized: bool,
}

impl Placement {
    /// Whether it's a size a window can sensibly reopen at.
    pub fn usable(&self) -> bool {
        [self.x, self.y, self.width, self.height]
            .iter()
            .all(|v| v.is_finite())
            && self.width >= 200.0
            && self.height >= 200.0
    }
}

pub fn path() -> PathBuf {
    data_dir().join("window.json")
}

/// The saved placement; `None` when there isn't one or it can't be read.
pub fn load() -> Option<Placement> {
    let raw = std::fs::read_to_string(path()).ok()?;
    serde_json::from_str::<Placement>(&raw)
        .ok()
        .filter(Placement::usable)
}

pub fn save(placement: &Placement) -> Result<(), String> {
    let path = path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("window: can't create {} ({e}).", dir.display()))?;
    }
    let raw = serde_json::to_string_pretty(placement)
        .map_err(|e| format!("window: placement not saved ({e})."))?;
    std::fs::write(&path, raw).map_err(|e| format!("window: can't write {} ({e}).", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unusable_placements() {
        let ok = Placement {
            x: 10.0,
            y: 20.0,
            width: 440.0,
            height: 720.0,
            maximized: false,
        };
        assert!(ok.usable());
        assert!(!Placement { width: 50.0, ..ok }.usable());
        assert!(!Placement { x: f32::NAN, ..ok }.usable());
        let round: Placement = serde_json::from_str(&serde_json::to_string(&ok).unwrap()).unwrap();
        assert_eq!(round, ok);
    }
}
