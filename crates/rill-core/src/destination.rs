//! Where the composer sends a message, and how Tab cycles through the choices.

use crate::Platform;

/// The composer's target: every connected platform, or one of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Destination {
    #[default]
    All,
    One(Platform),
}

impl Destination {
    /// The next destination (Tab): ALL, then each connected platform in order, then ALL again.
    pub fn next(self, connected: &[Platform]) -> Destination {
        self.step(connected, true)
    }

    /// The previous destination (Shift+Tab), the reverse of [`Destination::next`].
    pub fn prev(self, connected: &[Platform]) -> Destination {
        self.step(connected, false)
    }

    /// The platforms a message to this destination goes to.
    pub fn targets(self, connected: &[Platform]) -> Vec<Platform> {
        match self {
            Destination::All => cycle_order(connected),
            Destination::One(p) if connected.contains(&p) => vec![p],
            Destination::One(_) => Vec::new(),
        }
    }

    fn step(self, connected: &[Platform], forward: bool) -> Destination {
        let mut ring = vec![Destination::All];
        ring.extend(cycle_order(connected).into_iter().map(Destination::One));
        let len = ring.len();
        let Some(pos) = ring.iter().position(|d| *d == self) else {
            return Destination::All;
        };
        let pos = if forward {
            (pos + 1) % len
        } else {
            (pos + len - 1) % len
        };
        ring[pos]
    }
}

/// Connected platforms in the fixed cycle order, without duplicates.
fn cycle_order(connected: &[Platform]) -> Vec<Platform> {
    Platform::ALL
        .into_iter()
        .filter(|p| connected.contains(p))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use Platform::*;

    #[test]
    fn next_cycles_all_then_each_platform() {
        let c = [Kick, Twitch, YouTube];
        let mut d = Destination::All;
        let mut seen = Vec::new();
        for _ in 0..4 {
            d = d.next(&c);
            seen.push(d);
        }
        assert_eq!(
            seen,
            [
                Destination::One(Twitch),
                Destination::One(YouTube),
                Destination::One(Kick),
                Destination::All
            ]
        );
    }

    #[test]
    fn prev_reverses_next() {
        let c = [Twitch, YouTube, Kick];
        assert_eq!(Destination::All.prev(&c), Destination::One(Kick));
        assert_eq!(Destination::One(Twitch).prev(&c), Destination::All);
        for d in [Destination::All, Destination::One(YouTube)] {
            assert_eq!(d.next(&c).prev(&c), d);
        }
    }

    #[test]
    fn skips_disconnected_platforms() {
        let c = [Twitch, Kick];
        assert_eq!(Destination::One(Twitch).next(&c), Destination::One(Kick));
        assert_eq!(Destination::One(Kick).prev(&c), Destination::One(Twitch));
    }

    #[test]
    fn nothing_connected_stays_on_all() {
        assert_eq!(Destination::All.next(&[]), Destination::All);
        assert_eq!(Destination::All.prev(&[]), Destination::All);
    }

    #[test]
    fn disconnected_destination_falls_back_to_all() {
        assert_eq!(Destination::One(YouTube).next(&[Twitch]), Destination::All);
        assert!(Destination::One(YouTube).targets(&[Twitch]).is_empty());
    }

    #[test]
    fn all_targets_every_connected_platform() {
        assert_eq!(Destination::All.targets(&[Kick, Twitch]), [Twitch, Kick]);
    }
}
