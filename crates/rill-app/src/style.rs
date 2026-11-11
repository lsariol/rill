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

/// Rill's logo in its own colors: something that went to every platform.
pub fn rill_icon(size: f32) -> gpui::Img {
    gpui::img("icons/rill.svg").size(px(size)).flex_none()
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

/// A scroll bar that's always there, thin, and wider under the pointer.
pub fn scrollbar(bar: gpui_component::scroll::Scrollbar) -> gpui_component::scroll::Scrollbar {
    bar.mode(gpui_component::scroll::ScrollbarMode::Always)
        .styles(|s| {
            s.thumb(|t| t.width(px(6.)).inset(px(5.)))
                .thumb_hover(|t| t.width(px(12.)).inset(px(2.)))
                .thumb_active(|t| t.width(px(12.)).inset(px(2.)))
        })
}

/// Highlight colors to choose from in Settings (any other is a hex code away).
pub const HIGHLIGHT_COLORS: [u32; 10] = [
    0xf23f43, 0xf0883e, 0xf0b232, 0x23a55a, 0x1abc9c, 0x4da6ff, 0x7289da, 0xa970ff, 0xe05bb5,
    0x949ba4,
];

/// Name colors to choose from for Simple view.
pub const NAME_COLORS: [u32; 9] = [
    0xffffff, 0x5cb8ff, 0xff3b3b, 0xff6a2b, 0xff7ad9, 0xffd600, 0x3ddc84, 0xb57cff, 0xaab0b8,
];

/// Reads `#rrggbb` or `#rgb` (the `#` is optional).
pub fn parse_hex(s: &str) -> Option<u32> {
    let hex = s.trim().trim_start_matches('#');
    if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    match hex.len() {
        6 => u32::from_str_radix(hex, 16).ok(),
        3 => {
            let v = u32::from_str_radix(hex, 16).ok()?;
            let (r, g, b) = ((v >> 8) & 0xf, (v >> 4) & 0xf, v & 0xf);
            Some((r * 0x11) << 16 | (g * 0x11) << 8 | (b * 0x11))
        }
        _ => None,
    }
}

/// First-time chatters' highlight unless Settings says otherwise.
pub const FIRST_CHATTER: u32 = 0x23a55a;

/// Each kind of event's highlight unless Settings says otherwise.
pub fn event_default(kind: rill_core::EventKind) -> u32 {
    use rill_core::EventKind::*;
    match kind {
        Subscription => 0xa970ff,
        Gift => 0xe05bb5,
        Raid => 0x4da6ff,
        Money => 0xf0b232,
        Redemption => 0x1abc9c,
        Follow => 0x7289da,
        Announcement => 0xf0883e,
        AdBreak => 0x949ba4,
        Other => 0xa970ff,
    }
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
