//! Embedded assets: Rill's own icons first, then gpui-kit's icon set for the components.

use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString};

/// Every asset Rill ships inside its binary.
pub struct Assets;

const OWN: &[(&str, &[u8])] = &[
    (
        "icons/twitch.svg",
        include_bytes!("../assets/icons/twitch.svg"),
    ),
    (
        "icons/youtube.svg",
        include_bytes!("../assets/icons/youtube.svg"),
    ),
    ("icons/kick.svg", include_bytes!("../assets/icons/kick.svg")),
    (
        "icons/rumble.svg",
        include_bytes!("../assets/icons/rumble.svg"),
    ),
    (
        "icons/rill.svg",
        include_bytes!("../assets/app/rill-small.svg"),
    ),
    // Kick's role badges, which Kick draws in its page instead of sending an image.
    (
        "kick-badges/owner.svg",
        include_bytes!("../assets/kick-badges/owner.svg"),
    ),
    (
        "kick-badges/moderator.svg",
        include_bytes!("../assets/kick-badges/moderator.svg"),
    ),
    (
        "kick-badges/vip.svg",
        include_bytes!("../assets/kick-badges/vip.svg"),
    ),
    (
        "kick-badges/subscriber.svg",
        include_bytes!("../assets/kick-badges/subscriber.svg"),
    ),
    // Rill's role badges for platforms that send no image for them (YouTube, Rumble).
    (
        "badges/owner.svg",
        include_bytes!("../assets/badges/owner.svg"),
    ),
    (
        "badges/moderator.svg",
        include_bytes!("../assets/badges/moderator.svg"),
    ),
    ("badges/vip.svg", include_bytes!("../assets/badges/vip.svg")),
    (
        "badges/subscriber.svg",
        include_bytes!("../assets/badges/subscriber.svg"),
    ),
];

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some((_, bytes)) = OWN.iter().find(|(p, _)| *p == path) {
            return Ok(Some(Cow::Borrowed(bytes)));
        }
        gpui_kit_assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut out: Vec<SharedString> = OWN
            .iter()
            .filter(|(p, _)| p.starts_with(path))
            .map(|(p, _)| SharedString::from(*p))
            .collect();
        out.extend(gpui_kit_assets::Assets.list(path)?);
        Ok(out)
    }
}
