//! Rill's look: a dark charcoal base with restrained platform accents.

use gpui::{Hsla, Rgba, Svg, prelude::*, px, rgb, svg};
use rill_core::{Level, Platform};

pub const BACKGROUND: u32 = 0x1e1f22;
pub const PANEL: u32 = 0x2b2d31;
pub const RAISED: u32 = 0x35373c;
pub const BORDER: u32 = 0x3f4147;
pub const TEXT: u32 = 0xdcddde;
pub const MUTED: u32 = 0x949ba4;
pub const FAINT: u32 = 0x6d7178;
pub const LINK: u32 = 0x00a8fc;
pub const MENTION_BAR: u32 = 0xf23f43;
pub const OK: u32 = 0x23a55a;
pub const WARN: u32 = 0xf0b232;
pub const ERROR: u32 = 0xf23f43;
/// Selected text's background, drawn see-through.
pub const SELECTION: u32 = 0x3d7eff;

/// The rainbow outline for ALL, as stops from left to right.
pub const RAINBOW: [u32; 6] = [0xff4d4d, 0xffa64d, 0xffe14d, 0x53fc18, 0x4da6ff, 0xb84dff];

/// A platform's brand color.
pub fn platform_color(p: Platform) -> Rgba {
    rgb(match p {
        Platform::Twitch => 0x9146ff,
        Platform::YouTube => 0xff0000,
        Platform::Kick => 0x53fc18,
        Platform::Rumble => 0x85c742,
    })
}

/// A platform's logo, tinted in its brand color.
pub fn platform_icon(p: Platform, size: f32) -> Svg {
    let path = match p {
        Platform::Twitch => "icons/twitch.svg",
        Platform::YouTube => "icons/youtube.svg",
        Platform::Kick => "icons/kick.svg",
        Platform::Rumble => "icons/rumble.svg",
    };
    svg()
        .path(path)
        .size(px(size))
        .flex_none()
        .text_color(platform_color(p))
}

/// The color for a notice level.
pub fn level_color(level: Level) -> Rgba {
    rgb(match level {
        Level::Info => MUTED,
        Level::Warn => WARN,
        Level::Error => ERROR,
        Level::Event => TEXT,
    })
}

/// Highlight colors to choose from in Settings.
pub const HIGHLIGHT_COLORS: [u32; 8] = [
    0xf23f43, 0xf0883e, 0xf0b232, 0x23a55a, 0x1abc9c, 0x4da6ff, 0xa970ff, 0xe05bb5,
];

/// Reads `#rrggbb` (or `rrggbb`).
pub fn parse_hex(s: &str) -> Option<u32> {
    let hex = s.trim().trim_start_matches('#');
    (hex.len() == 6)
        .then(|| u32::from_str_radix(hex, 16).ok())
        .flatten()
}

/// Writes a color as `#rrggbb`.
pub fn to_hex(color: u32) -> String {
    format!("#{color:06x}")
}

/// A color from a hex value.
pub fn hsla(hex: u32) -> Hsla {
    rgb(hex).into()
}

/// A color from a hex value, see-through by `alpha` (0 to 1).
pub fn hsla_alpha(hex: u32, alpha: f32) -> Hsla {
    let mut c = hsla(hex);
    c.a = alpha;
    c
}

/// A user's name color, lightened if it would be hard to read on the dark background.
pub fn readable_name(color: Option<u32>) -> Hsla {
    let mut c = hsla(color.unwrap_or(0x9b9bff));
    if c.l < 0.6 {
        c.l = 0.6 + (c.l * 0.2);
    }
    c
}
