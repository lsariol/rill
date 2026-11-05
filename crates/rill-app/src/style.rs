//! Rill's colors: a dark charcoal base with restrained platform accents.

use gpui::{Hsla, Rgba, rgb};
use rill_core::Platform;

pub const BACKGROUND: u32 = 0x1e1f22;
pub const PANEL: u32 = 0x2b2d31;
pub const TEXT: u32 = 0xdcddde;
pub const MUTED: u32 = 0x949ba4;

/// The rainbow outline for ALL, as stops from left to right.
pub const RAINBOW: [u32; 6] = [0xff4d4d, 0xffa64d, 0xffe14d, 0x53fc18, 0x4da6ff, 0xb84dff];

/// A platform's brand color.
pub fn platform_color(p: Platform) -> Rgba {
    rgb(match p {
        Platform::Twitch => 0x9146ff,
        Platform::YouTube => 0xff0000,
        Platform::Kick => 0x53fc18,
    })
}

/// A color from a hex value.
pub fn hsla(hex: u32) -> Hsla {
    rgb(hex).into()
}
