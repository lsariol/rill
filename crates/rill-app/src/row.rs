//! Feed rows: chat messages, the user's own sent messages, and notices.

use std::ops::Range;

use chrono::TimeZone;
use gpui::{
    AnyElement, Context, ElementId, FontStyle, FontWeight, HighlightStyle, InteractiveElement,
    IntoElement, ParentElement, StatefulInteractiveElement, Styled, StyledImage, StyledText,
    UnderlineStyle, Window, div, img, prelude::FluentBuilder, px, rgb,
};
use rill_core::feed::{DeliveryState, Notice, SentMessage};
use rill_core::{Badge, ChatMessage, Entry, Fragment, Item, MessageKind, Platform};

use crate::chat_view::ChatView;
use crate::panels::{Panel, UserCard};
use crate::style;

const EMOTE: f32 = 22.;
const ICON: f32 = 14.;

impl ChatView {
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
                row_frame(entry.seq, entry.highlighted, compact)
                    .child(gutter(Some(m.platform)))
                    .when(!compact, |d| d.child(time(entry.received_ms)))
                    .when(!compact && !m.author.badges.is_empty(), |d| {
                        d.child(badges(&m.author.badges))
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .children(quote)
                            .child(chat_body(m, entry.deleted)),
                    )
                    .cursor_pointer()
                    .on_click(cx.listener(move |view, _, window, cx| {
                        view.open_panel(Panel::User(card.clone()), window, cx);
                    }))
                    .into_any_element()
            }
            Item::Sent(s) => {
                let name = s
                    .deliveries
                    .iter()
                    .find_map(|d| me(d.platform))
                    .unwrap_or_else(|| "You".into());
                sent_row(entry, s, &name, compact).into_any_element()
            }
            Item::Notice(n) => notice_row(entry, n, compact).into_any_element(),
        }
    }
}

fn row_frame(seq: u64, highlighted: bool, compact: bool) -> gpui::Stateful<gpui::Div> {
    div()
        .id(ElementId::NamedInteger("row".into(), seq))
        .w_full()
        .flex()
        .items_start()
        .gap_1p5()
        .px_3()
        .py(px(if compact { 1. } else { 3. }))
        .border_l_2()
        .border_color(rgb(if highlighted {
            style::MENTION_BAR
        } else {
            style::BACKGROUND
        }))
        .when(highlighted, |d| d.bg(rgb(style::MENTION_BG)))
        .hover(|s| s.bg(rgb(style::PANEL)))
}

/// The platform icon column, aligned with the first line of text.
fn gutter(platform: Option<Platform>) -> impl IntoElement {
    div()
        .pt(px(3.))
        .w(px(ICON))
        .flex_none()
        .children(platform.map(|p| style::platform_icon(p, ICON)))
}

fn time(ms: i64) -> impl IntoElement {
    let text = chrono::Local
        .timestamp_millis_opt(ms)
        .single()
        .map(|t| t.format("%H:%M").to_string())
        .unwrap_or_default();
    div()
        .flex_none()
        .text_xs()
        .pt(px(2.))
        .text_color(rgb(style::FAINT))
        .child(text)
}

fn badges(badges: &[Badge]) -> impl IntoElement {
    div()
        .flex()
        .flex_none()
        .gap(px(2.))
        .pt(px(3.))
        .children(badges.iter().take(4).map(|b| match &b.image_url {
            Some(url) => img(url.clone()).size(px(ICON)).into_any_element(),
            None => badge_chip(b).into_any_element(),
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
        .size(px(ICON))
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

/// The name and message text; images flow word by word, plain text wraps as one run.
fn chat_body(m: &ChatMessage, deleted: bool) -> AnyElement {
    let name_color = style::readable_name(m.author.color);
    let action = m.kind == MessageKind::Action;
    if deleted {
        let name = m.author.display_name.clone();
        let text = format!("{name}  message deleted");
        let highlights = vec![
            (0..name.len(), bold(name_color)),
            (
                name.len()..text.len(),
                HighlightStyle {
                    color: Some(style::hsla(style::FAINT)),
                    font_style: Some(FontStyle::Italic),
                    ..HighlightStyle::default()
                },
            ),
        ];
        return StyledText::new(text)
            .with_highlights(highlights)
            .into_any_element();
    }
    if !m.has_images() {
        let (text, highlights) = styled_message(m, name_color, action);
        return StyledText::new(text)
            .with_highlights(highlights)
            .into_any_element();
    }
    // Actions and platform events read as one sentence: "Bob subscribed", not "Bob: subscribed".
    let sentence = action || m.kind == MessageKind::Event;
    let sep = if sentence { "" } else { ":" };
    let mut words: Vec<AnyElement> = vec![
        div()
            .font_weight(FontWeight::BOLD)
            .text_color(name_color)
            .child(format!("{}{sep}", m.author.display_name))
            .into_any_element(),
    ];
    // The last emote drawn, which a zero-width emote after it sits on.
    let mut base: Option<usize> = None;
    for f in &m.fragments {
        match f {
            Fragment::Text(t) => {
                if !t.trim().is_empty() {
                    base = None;
                }
                words.extend(t.split_whitespace().map(|w| {
                    div()
                        .when(action, |d| d.italic().text_color(name_color))
                        .child(w.to_string())
                        .into_any_element()
                }));
            }
            Fragment::Emote {
                name,
                url,
                size,
                overlay,
            } => {
                let image = emote_image(name, url.as_deref(), *size);
                match base {
                    Some(i) if *overlay => {
                        let under = std::mem::replace(&mut words[i], div().into_any_element());
                        words[i] = div()
                            .relative()
                            .flex_none()
                            .child(under)
                            .child(
                                div()
                                    .absolute()
                                    .inset_0()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .child(image),
                            )
                            .into_any_element();
                    }
                    _ => {
                        words.push(image);
                        base = Some(words.len() - 1);
                    }
                }
            }
            Fragment::Mention(n) => {
                base = None;
                words.push(
                    div()
                        .font_weight(FontWeight::BOLD)
                        .child(format!("@{n}"))
                        .into_any_element(),
                );
            }
            Fragment::Link(l) => {
                base = None;
                words.push(
                    div()
                        .text_color(rgb(style::LINK))
                        .underline()
                        .child(l.clone())
                        .into_any_element(),
                );
            }
        }
    }
    div()
        .w_full()
        .flex()
        .flex_wrap()
        .items_center()
        .gap_x(px(4.))
        .children(words)
        .into_any_element()
}

/// An emote image at its own proportions (third-party emotes vary), or its name without one.
fn emote_image(name: &str, url: Option<&str>, size: Option<(u16, u16)>) -> AnyElement {
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
        // 7TV's 1x images are 32 px tall where Twitch's are 28: keep the same ratio to EMOTE.
        Some((w, h)) => {
            let scale = EMOTE / 28.;
            image
                .w(px(f32::from(w) * scale))
                .h(px(f32::from(h.min(32)) * scale))
                .into_any_element()
        }
        None => image.h(px(EMOTE)).into_any_element(),
    }
}

/// The whole message as one string with styled ranges.
fn styled_message(
    m: &ChatMessage,
    name_color: gpui::Hsla,
    action: bool,
) -> (String, Vec<(Range<usize>, HighlightStyle)>) {
    let mut text = m.author.display_name.clone();
    let mut highlights = vec![(0..text.len(), bold(name_color))];
    let sentence = action || m.kind == MessageKind::Event;
    text.push_str(if sentence { " " } else { ": " });
    let body_start = text.len();
    for f in &m.fragments {
        let start = text.len();
        let style = match f {
            Fragment::Text(t) => {
                text.push_str(t);
                None
            }
            Fragment::Emote { name, .. } => {
                text.push_str(name);
                Some(bold(style::hsla(style::MUTED)))
            }
            Fragment::Mention(n) => {
                text.push('@');
                text.push_str(n);
                Some(bold(style::hsla(0xffffff)))
            }
            Fragment::Link(l) => {
                text.push_str(l);
                Some(HighlightStyle {
                    color: Some(style::hsla(style::LINK)),
                    underline: Some(UnderlineStyle {
                        thickness: px(1.),
                        ..UnderlineStyle::default()
                    }),
                    ..HighlightStyle::default()
                })
            }
        };
        if let Some(style) = style {
            highlights.push((start..text.len(), style));
        }
    }
    if action {
        highlights.truncate(1);
        highlights.push((
            body_start..text.len(),
            HighlightStyle {
                color: Some(name_color),
                font_style: Some(FontStyle::Italic),
                ..HighlightStyle::default()
            },
        ));
    }
    (text, highlights)
}

fn bold(color: gpui::Hsla) -> HighlightStyle {
    HighlightStyle {
        color: Some(color),
        font_weight: Some(FontWeight::BOLD),
        ..HighlightStyle::default()
    }
}

fn sent_row(entry: &Entry, s: &SentMessage, name: &str, compact: bool) -> impl IntoElement {
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
            .child(style::platform_icon(d.platform, ICON))
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
    let text = format!("{name}: {}", s.text);
    let highlights = vec![(0..name.len(), bold(style::hsla(0x53fc18)))];
    row_frame(entry.seq, false, compact)
        .child(div().flex().flex_none().gap_1().pt(px(1.)).children(marks))
        .when(!compact, |d| d.child(time(entry.received_ms)))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(StyledText::new(text).with_highlights(highlights))
                .children(failures),
        )
}

fn notice_row(entry: &Entry, n: &Notice, compact: bool) -> impl IntoElement {
    row_frame(entry.seq, false, compact)
        .child(gutter(n.platform))
        .when(!compact, |d| d.child(time(entry.received_ms)))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_color(style::level_color(n.level))
                .when(n.level == rill_core::Level::Info, |d| d.italic())
                .children(n.text.lines().map(|l| div().child(l.to_string()))),
        )
}
