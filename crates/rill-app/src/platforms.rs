//! Which platforms are turned on, and how to start one.

use std::collections::BTreeSet;

use rill_core::{Adapter, Platform};

/// Makes a platform's adapter.
pub type MakeAdapter = Box<dyn Fn(Platform) -> Box<dyn Adapter>>;

/// The platforms in use. Turned-off ones aren't connected and don't show anywhere.
pub struct Platforms {
    off: BTreeSet<Platform>,
    make: MakeAdapter,
    /// Whether changes go to settings.json; simulated runs leave it alone.
    save: bool,
}

impl Platforms {
    pub fn new(off: BTreeSet<Platform>, make: MakeAdapter, save: bool) -> Platforms {
        Platforms { off, make, save }
    }

    /// The names in settings.json's list, ignoring ones Rill doesn't know.
    pub fn parse(names: &[String]) -> BTreeSet<Platform> {
        names
            .iter()
            .filter_map(|n| Platform::from_name(n))
            .collect()
    }

    /// Whether settings changes are saved (not in simulated runs).
    pub fn saves(&self) -> bool {
        self.save
    }

    pub fn is_on(&self, p: Platform) -> bool {
        !self.off.contains(&p)
    }

    /// The platforms turned on, in display order.
    pub fn on(&self) -> Vec<Platform> {
        Platform::ALL
            .into_iter()
            .filter(|p| self.is_on(*p))
            .collect()
    }

    /// A fresh adapter for the platform.
    pub fn make(&self, p: Platform) -> Box<dyn Adapter> {
        (self.make)(p)
    }

    /// Adapters for every platform that's on, for startup.
    pub fn start(&self) -> Vec<Box<dyn Adapter>> {
        self.on().into_iter().map(|p| self.make(p)).collect()
    }

    /// Turns a platform on or off and saves the choice.
    pub fn set(&mut self, p: Platform, on: bool) -> Result<(), String> {
        if on {
            self.off.remove(&p);
        } else {
            self.off.insert(p);
        }
        if !self.save {
            return Ok(());
        }
        let names = self.off.iter().map(|p| p.name().to_lowercase()).collect();
        rill_os::settings::update(|s| s.disabled_platforms = names)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_names_in_any_case_and_skips_unknown_ones() {
        let off = Platforms::parse(&["Rumble".into(), "kick".into(), "tiktok".into()]);
        assert_eq!(off, BTreeSet::from([Platform::Kick, Platform::Rumble]));
    }
}
