//! Feed rows: chat messages, the user's own sent messages, and notices.

use std::rc::Rc;

use chrono::TimeZone;
use gpui::{
    AnyElement, Context, ElementId, FontStyle, FontWeight, HighlightStyle, InteractiveElement,
    IntoElement, ParentElement, StatefulInteractiveElement, Styled, StyledImage, Window, div, img,
    prelude::FluentBuilder, px, rgb,
};
use rill_core::feed::{DeliveryState, Notice, SentMessage};
use rill_core::{Badge, ChatMessage, Entry, Fragment, Item, MessageKind, Platform};

use crate::chat_text::{self, ChatText, ClickFn, Hover, HoverFn, Parts, Target};
use crate::chat_view::ChatView;
use crate::panels::{Panel, UserCard};
use crate::style;
use gpui::SharedString;

/// The text size chosen in Settings, in percent; emotes, icons and badges follow it.
static SCALE_PCT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(100);

/// Sets the size emotes, icons and badges are drawn at, in percent of normal.
pub(crate) fn set_scale(pct: u32) {
    SCALE_PCT.store(pct, std::sync::atomic::Ordering::Relaxed);
}

pub(crate) fn scale() -> f32 {
    SCALE_PCT.load(std::sync::atomic::Ordering::Relaxed) as f32 / 100.
}

/// An emote's height in chat.
pub(crate) fn emote_px() -> f32 {
    22. * scale()
}

/// Platform icons and badges.
fn icon_px() -> f32 {
    14. * scale()
}

impl ChatView {
    /// Tells the view when the pointer moves onto or off a link, emote or badge, for its card.
    pub(crate) fn link_hover_fn(&self, cx: &mut Context<Self>) -> HoverFn {
        let view = cx.entity().downgrade();
        Rc::new(move |source, hover, _, cx| {
            let _ = view.update(cx, |view, cx| view.set_hover(source, hover, cx));
        })
    }

    /// A chat message's selectable text; clicking the name opens the user card.
    fn chat_text(
        &self,
        entry: &Entry,
        m: &ChatMessage,
        card: UserCard,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (seq, deleted) = (entry.seq, entry.deleted);
        let soft = deleted && self.show_deleted;
        let parts = chat_parts(m, deleted && !soft);
        let view = cx.entity().downgrade();
        let hover = self.link_hover_fn(cx);
        let text = ChatText::new(("chat-text", seq), order(seq, 1), parts)
            .copy_prefix(copy_time(entry.received_ms))
            .on_hover(hover)
            .on_click(Rc::new(move |target, at, window, cx| match target {
                Target::Name => {
                    let card = card.clone();
                    let _ = view.update(cx, |view, cx| view.open_user_card(card, at, window, cx));
                }
                Target::Link(url) => cx.open_url(url),
                Target::Emote(_) => {}
            }));
        if !soft {
            return text.into_any_element();
        }
        // The text stays readable for moderators, but clearly gone from the platform.
        div()
            .flex()
            .items_start()
            .gap_1p5()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .opacity(0.45)
                    .italic()
                    .line_through()
                    .child(text),
            )
            .child(
                div()
                    .flex_none()
                    .pt(px(2.))
                    .text_xs()
                    .text_color(rgb(style::FAINT))
                    .child("deleted"),
            )
            .into_any_element()
    }

    /// Opens the user card for a name clicked at `at` (window coordinates), floating beside it.
    pub(crate) fn open_user_card(
        &mut self,
        card: UserCard,
        at: gpui::Point<gpui::Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_panel(Panel::User(card), window, cx);
        self.card_at = Some(at.y);
    }

    /// Renders the visible entry at `ix`, outlined when it's picked with the keyboard.
    pub(crate) fn render_row(
        &mut self,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let row = self.row_element(ix, cx);
        let picked = self.selected.is_some()
            && self.session.feed.visible(ix).map(|e| e.seq) == self.selected
            && self.feed_focused(window, cx);
        if !picked {
            return row;
        }
        div()
            .w_full()
            .bg(rgb(style::RAISED))
            .border_l_2()
            .border_color(rgb(style::LINK))
            .child(row)
            .into_any_element()
    }

    fn row_element(&mut self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        self.stats.rows += 1;
        let compact = self.is_compact();
        let Some(entry) = self.session.feed.visible(ix) else {
            return div().into_any_element();
        };
        let me = |p: Platform| {
            self.session
                .link(p)
                .and_then(|l| l.account.as_ref())
                .map(|a| a.display_name.clone())
        };
        match &entry.item {
            Item::Chat(m) => {
                let card = UserCard {
                    platform: m.platform,
                    author: m.author.clone(),
                    message_id: Some(m.id.clone()),
                    deleted: entry.deleted,
                    text: m.plain_text(),
                };
                // What a reply answers, quoted above it.
                let quote = m.reply_to.as_ref().map(|r| {
                    let text: String = r.text.chars().take(80).collect();
                    div()
                        .text_xs()
                        .text_color(rgb(style::FAINT))
                        .truncate()
                        .child(format!("↩ @{}: {text}", r.author))
                });
                // The message the open card is about stays marked while chat moves on.
                let carded = matches!(&self.panel, Panel::User(c)
                    if c.platform == m.platform && c.message_id.as_deref() == Some(m.id.as_str()));
                row_frame(entry.seq, self.highlight_color(entry), compact)
                    .when(carded, |d| d.bg(rgb(style::RAISED)))
                    .when(!compact, |d| d.child(time(entry.seq, entry.received_ms)))
                    .child(gutter(Some(m.platform)))
                    .when(!compact && !m.author.badges.is_empty(), |d| {
                        d.child(badges(entry.seq, &m.author.badges, self.link_hover_fn(cx)))
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .children(quote)
                            .child(self.chat_text(entry, m, card, cx)),
                    )
                    .into_any_element()
            }
            Item::Sent(s) => {
                let name = s
                    .deliveries
                    .iter()
                    .find_map(|d| me(d.platform))
                    .unwrap_or_else(|| "You".into());
                sent_row(entry, s, &name, compact, self.link_hover_fn(cx)).into_any_element()
            }
            Item::Notice(n) => notice_row(
                entry,
                n,
                compact,
                self.highlight_color(entry),
                self.link_hover_fn(cx),
            )
            .into_any_element(),
        }
    }
}

/// A row: a colored bar and tint when it's highlighted (a mention or an event).
fn row_frame(seq: u64, highlight: Option<u32>, compact: bool) -> gpui::Stateful<gpui::Div> {
    div()
        .id(ElementId::NamedInteger("row".into(), seq))
        .w_full()
        .flex()
        .items_start()
        .gap_1p5()
        .px_3()
        .py(px(if compact { 1. } else { 3. }))
        .border_l_2()
        .border_color(rgb(highlight.unwrap_or(style::BACKGROUND)))
        .when_some(highlight, |d, color| d.bg(style::hsla_alpha(color, 0.2)))
        .hover(|s| s.bg(rgb(style::PANEL)))
}

/// The platform icon column, aligned with the first line of text.
fn gutter(platform: Option<Platform>) -> impl IntoElement {
    div()
        .pt(px(3.))
        .w(px(icon_px()))
        .flex_none()
        .children(platform.map(|p| style::platform_icon(p, icon_px())))
}

/// A time as the row shows it.
fn clock(ms: i64) -> String {
    chrono::Local
        .timestamp_millis_opt(ms)
        .single()
        .map(|t| t.format("%H:%M").to_string())
        .unwrap_or_default()
}

/// What a row's text copies with in front of it: its time.
fn copy_time(ms: i64) -> String {
    format!("{} ", clock(ms))
}

/// The time column, at the far left. A drag can start on it; the line beside it copies the time.
fn time(seq: u64, ms: i64) -> impl IntoElement {
    let mut parts = Parts::default();
    parts.text(clock(ms), None);
    div()
        .flex_none()
        .text_xs()
        .pt(px(2.))
        .text_color(rgb(style::FAINT))
        .child(ChatText::new(("time", seq), order(seq, 0), parts).silent())
}

/// Badges, each saying what it is on hover ("Subscriber · 1 year, 4 months"), straight away.
fn badges(seq: u64, badges: &[Badge], hover: HoverFn) -> impl IntoElement {
    div()
        .flex()
        .flex_none()
        .gap(px(2.))
        .pt(px(3.))
        .children(badges.iter().take(4).enumerate().map(|(i, b)| {
            let image = match &b.image_url {
                // FrankerFaceZ's custom moderator badges are drawn on moderator green.
                Some(url) if b.id.starts_with("moderator/") && url.contains("frankerfacez.com") => {
                    div()
                        .size(px(icon_px()))
                        .rounded_sm()
                        .bg(rgb(0x34ae0a))
                        .child(img(url.clone()).size(px(icon_px())))
                        .into_any_element()
                }
                Some(url) => img(url.clone()).size(px(icon_px())).into_any_element(),
                None => badge_chip(b).into_any_element(),
            };
            let (title, url) = (b.title.clone(), b.image_url.clone());
            let key = SharedString::from(format!("badge-{seq}-{i}"));
            let hover = hover.clone();
            div()
                .id(ElementId::NamedInteger(format!("badge-{i}").into(), seq))
                .child(image)
                .on_hover(move |on, window, cx| {
                    let card = on.then(|| Hover::Badge {
                        title: title.clone(),
                        url: url.clone(),
                        at: window.mouse_position(),
                    });
                    hover(key.clone(), card, window, cx);
                })
        }))
}

/// A lettered stand-in for a badge without an image.
fn badge_chip(b: &Badge) -> impl IntoElement {
    let kind = b.id.split('/').next().unwrap_or("");
    let (letter, color) = match kind {
        "broadcaster" => ("B", 0xe91916),
        "moderator" => ("M", 0x00ad03),
        "vip" => ("V", 0xe005b9),
        "subscriber" => ("S", 0x8205b4),
        _ => ("•", 0x4e5058),
    };
    div()
        .size(px(icon_px()))
        .rounded_sm()
        .bg(rgb(color))
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(9.))
        .font_weight(FontWeight::BOLD)
        .text_color(rgb(0xffffff))
        .child(letter)
}

/// The name and message text as one selectable line. The name opens the user card.
fn chat_parts(m: &ChatMessage, deleted: bool) -> Parts {
    let name_color = style::readable_name(m.author.color);
    let mut parts = Parts::default();
    parts.target(
        m.author.display_name.clone(),
        bold(name_color),
        Target::Name,
    );
    if deleted {
        parts.text(
            "  message deleted",
            Some(HighlightStyle {
                color: Some(style::hsla(style::FAINT)),
                font_style: Some(FontStyle::Italic),
                ..HighlightStyle::default()
            }),
        );
        return parts;
    }
    let action = m.kind == MessageKind::Action;
    // Actions and platform events read as one sentence: "Bob subscribed", not "Bob: subscribed".
    let sentence = action || m.kind.is_event();
    parts.text(if sentence { " " } else { ": " }, None);
    let plain = action.then_some(HighlightStyle {
        color: Some(name_color),
        font_style: Some(FontStyle::Italic),
        ..HighlightStyle::default()
    });
    fragment_parts(&mut parts, &m.fragments, plain);
    parts
}

/// Message fragments as parts: emote images, mentions, and links (found in plain text too).
fn fragment_parts(parts: &mut Parts, fragments: &[Fragment], plain: Option<HighlightStyle>) {
    for f in fragments {
        match f {
            Fragment::Text(t) => parts.linked(t, plain),
            Fragment::Emote {
                name,
                url: Some(url),
                size,
                overlay,
                scope,
            } => parts.emote(name, url, *size, *overlay, *scope),
            Fragment::Emote { name, .. } => {
                parts.text(name.clone(), Some(bold(style::hsla(style::MUTED))))
            }
            Fragment::Mention(n) => parts.text(format!("@{n}"), Some(bold(style::hsla(0xffffff)))),
            Fragment::Link(l) => match chat_text::link_in(l) {
                Some((url, _)) => parts.link(l, url),
                None => parts.text(l.clone(), plain),
            },
        }
    }
}

/// Opens links from any line; names only do something on chat lines.
fn open_links() -> ClickFn {
    Rc::new(|target, _, _, cx| {
        if let Target::Link(url) = target {
            cx.open_url(url);
        }
    })
}

/// A message's text without its author, for the user card's history.
pub(crate) fn history_text(seq: u64, m: &ChatMessage, hover: HoverFn) -> ChatText {
    let mut parts = Parts::default();
    fragment_parts(&mut parts, &m.fragments, None);
    ChatText::new(("history-text", seq), order(seq, 1), parts)
        .on_click(open_links())
        .on_hover(hover)
}

/// A line's place among the others, for copying several in order.
fn order(seq: u64, line: usize) -> u64 {
    seq * 64 + line.min(63) as u64
}

/// The size an emote is drawn at: its own proportions (third-party emotes vary), or square.
pub(crate) fn emote_size(size: Option<(u16, u16)>) -> gpui::Size<gpui::Pixels> {
    // 7TV's 1x images are 32 px tall where Twitch's are 28: keep the same ratio to an emote.
    let scale = emote_px() / 28.;
    match size {
        Some((w, h)) => gpui::size(px(f32::from(w) * scale), px(f32::from(h.min(32)) * scale)),
        None => gpui::size(px(emote_px()), px(emote_px())),
    }
}

/// Which service an emote comes from, by its image's address.
pub(crate) fn emote_service(url: &str) -> Option<&'static str> {
    let host = url.split("://").nth(1).unwrap_or(url);
    if host.starts_with("static-cdn.jtvnw.net") {
        Some("Twitch")
    } else if host.contains("7tv.app") {
        Some("7TV")
    } else if host.contains("betterttv.net") {
        Some("BetterTTV")
    } else if host.contains("frankerfacez.com") {
        Some("FrankerFaceZ")
    } else if host.contains("kick.com") {
        Some("Kick")
    } else if host.contains("ggpht.com") || host.contains("youtube.com") {
        Some("YouTube")
    } else if host.contains("rumble.com") {
        Some("Rumble")
    } else {
        None
    }
}

/// An emote's origin for its hover card: "7TV channel emote", "Twitch global emote".
pub(crate) fn emote_source(url: &str, scope: rill_core::EmoteScope) -> String {
    use rill_core::EmoteScope::*;
    let service = emote_service(url)
        .map(|s| format!("{s} "))
        .unwrap_or_default();
    match scope {
        Channel => format!("{service}channel emote"),
        Global => format!("{service}global emote"),
        OtherChannel => format!("{service}emote from another channel"),
        Unknown => format!("{service}emote"),
    }
}

/// The address of a bigger copy of an emote or badge image, where the service has one.
pub(crate) fn bigger_image(url: &str) -> String {
    let swap = |from: &str, to: &str| url.strip_suffix(from).map(|base| format!("{base}{to}"));
    if url.contains("static-cdn.jtvnw.net/emoticons/") {
        swap("/1.0", "/3.0").or_else(|| swap("/2.0", "/3.0"))
    } else if url.contains("static-cdn.jtvnw.net/badges/") {
        swap("/2", "/3").or_else(|| swap("/1", "/3"))
    } else if url.contains("7tv.app") {
        swap("/1x.webp", "/3x.webp").or_else(|| swap("/1x.avif", "/3x.avif"))
    } else if url.contains("betterttv.net") {
        swap("/1x.webp", "/3x.webp").or_else(|| swap("/1x", "/3x"))
    } else if url.contains("frankerfacez.com/emote/") {
        swap("/1", "/4")
    } else {
        None
    }
    .unwrap_or_else(|| url.to_string())
}

/// An emote image at its own proportions (third-party emotes vary), or its name without one.
pub(crate) fn emote_image(name: &str, url: Option<&str>, size: Option<(u16, u16)>) -> AnyElement {
    let label = |name: String| {
        div()
            .font_weight(FontWeight::BOLD)
            .text_color(rgb(style::MUTED))
            .child(name)
            .into_any_element()
    };
    let Some(url) = url else {
        return label(name.to_string());
    };
    let fallback = name.to_string();
    let image = img(url.to_string())
        .flex_none()
        .with_fallback(move || label(fallback.clone()));
    match size {
        Some(_) => {
            let s = emote_size(size);
            image.w(s.width).h(s.height).into_any_element()
        }
        None => image.h(px(emote_px())).into_any_element(),
    }
}

fn bold(color: gpui::Hsla) -> HighlightStyle {
    HighlightStyle {
        color: Some(color),
        font_weight: Some(FontWeight::BOLD),
        ..HighlightStyle::default()
    }
}

fn sent_row(
    entry: &Entry,
    s: &SentMessage,
    name: &str,
    compact: bool,
    hover: HoverFn,
) -> impl IntoElement {
    let marks = s.deliveries.iter().map(|d| {
        let (mark, color) = match d.state {
            DeliveryState::Pending => ("…", style::FAINT),
            DeliveryState::Sent => ("✓", style::OK),
            DeliveryState::Failed(_) => ("✗", style::ERROR),
        };
        div()
            .flex()
            .items_center()
            .gap(px(1.))
            .child(style::platform_icon(d.platform, icon_px()))
            .child(div().text_xs().text_color(rgb(color)).child(mark))
    });
    let failures = s.deliveries.iter().filter_map(|d| match &d.state {
        DeliveryState::Failed(why) => {
            let p = d.platform.name();
            let line = if why.starts_with(p) {
                format!("✗ {why}")
            } else {
                format!("✗ {p}: {why}")
            };
            Some(div().text_xs().text_color(rgb(style::ERROR)).child(line))
        }
        _ => None,
    });
    let mut parts = Parts::default();
    parts.text(format!("{name}: "), Some(bold(style::hsla(0x53fc18))));
    if s.fragments.is_empty() {
        parts.linked(&s.text, None);
    } else {
        fragment_parts(&mut parts, &s.fragments, None);
    }
    let body = ChatText::new(("sent-text", entry.seq), order(entry.seq, 1), parts)
        .copy_prefix(copy_time(entry.received_ms))
        .on_click(open_links())
        .on_hover(hover);
    // One mark in the icon column, like received messages, so the text lines up with them.
    let mark = match s.deliveries.as_slice() {
        [one] => style::platform_icon(one.platform, icon_px()).into_any_element(),
        _ => all_mark().into_any_element(),
    };
    row_frame(entry.seq, None, compact)
        .when(!compact, |d| d.child(time(entry.seq, entry.received_ms)))
        .child(div().pt(px(3.)).w(px(icon_px())).flex_none().child(mark))
        .child(div().flex_1().min_w_0().child(body).children(failures))
        .child(div().flex().flex_none().gap_1().pt(px(1.)).children(marks))
}

/// A small rainbow square: sent to every platform.
fn all_mark() -> impl IntoElement {
    div()
        .size(px(icon_px()))
        .rounded_sm()
        .overflow_hidden()
        .flex()
        .children(
            style::RAINBOW
                .iter()
                .map(|c| div().flex_1().h_full().bg(rgb(*c))),
        )
}

fn notice_row(
    entry: &Entry,
    n: &Notice,
    compact: bool,
    highlight: Option<u32>,
    hover: HoverFn,
) -> impl IntoElement {
    row_frame(entry.seq, highlight, compact)
        .when(!compact, |d| d.child(time(entry.seq, entry.received_ms)))
        .child(gutter(n.platform))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_color(style::level_color(n.level))
                .when(
                    matches!(n.level, rill_core::Level::Info | rill_core::Level::Event),
                    |d| d.italic(),
                )
                .children(n.text.lines().enumerate().map(|(i, l)| {
                    let mut parts = Parts::default();
                    parts.linked(l, None);
                    let line = ChatText::new(
                        ElementId::NamedInteger(format!("notice-{i}").into(), entry.seq),
                        order(entry.seq, i + 1),
                        parts,
                    );
                    let line = if i == 0 {
                        line.copy_prefix(copy_time(entry.received_ms))
                    } else {
                        line
                    };
                    line.on_click(open_links()).on_hover(hover.clone())
                })),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bigger_images_where_the_service_has_them() {
        let twitch = "https://static-cdn.jtvnw.net/emoticons/v2/25/default/dark/1.0";
        assert_eq!(
            bigger_image(twitch),
            "https://static-cdn.jtvnw.net/emoticons/v2/25/default/dark/3.0"
        );
        assert_eq!(
            bigger_image("https://cdn.7tv.app/emote/abc/1x.webp"),
            "https://cdn.7tv.app/emote/abc/3x.webp"
        );
        assert_eq!(
            bigger_image("https://cdn.betterttv.net/emote/b1/1x.webp"),
            "https://cdn.betterttv.net/emote/b1/3x.webp"
        );
        assert_eq!(
            bigger_image("https://cdn.frankerfacez.com/emote/9/1"),
            "https://cdn.frankerfacez.com/emote/9/4"
        );
        assert_eq!(
            bigger_image("https://static-cdn.jtvnw.net/badges/v1/abc/2"),
            "https://static-cdn.jtvnw.net/badges/v1/abc/3"
        );
        let kick = "https://files.kick.com/emotes/1/fullsize";
        assert_eq!(bigger_image(kick), kick, "already full size");
    }

    #[test]
    fn says_where_an_emote_comes_from() {
        use rill_core::EmoteScope::*;
        let twitch = "https://static-cdn.jtvnw.net/emoticons/v2/25/default/dark/1.0";
        assert_eq!(emote_source(twitch, Global), "Twitch global emote");
        assert_eq!(
            emote_source(twitch, OtherChannel),
            "Twitch emote from another channel"
        );
        assert_eq!(
            emote_source("https://cdn.7tv.app/emote/a/1x.webp", Channel),
            "7TV channel emote"
        );
        assert_eq!(
            emote_source("https://files.kick.com/emotes/1/fullsize", Unknown),
            "Kick emote"
        );
        assert_eq!(emote_source("https://example.com/x.png", Unknown), "emote");
    }
}
