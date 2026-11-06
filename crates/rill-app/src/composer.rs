//! The composer: destination outline and label, Tab cycling, `/` and `:` autocomplete and
//! submitting.

use std::ops::Range;

use gpui::{
    App, Context, InteractiveElement, IntoElement, KeyBinding, ParentElement,
    StatefulInteractiveElement, Styled, Window, actions, div, linear_color_stop, linear_gradient,
    prelude::FluentBuilder, px, rgb,
};
use gpui_component::input::{Position, Textarea};
use rill_core::command::{self, CommandSpec};
use rill_core::{Destination, EmoteOption, Platform, Submitted, now_ms};

use crate::chat_view::ChatView;
use crate::panels::Panel;
use crate::{row, style};

/// The message the composer replies to.
#[derive(Debug, Clone)]
pub struct Replying {
    pub platform: rill_core::Platform,
    pub message_id: String,
    pub author: String,
    /// The message's text, quoted above the composer.
    pub text: String,
}

actions!(
    composer,
    [
        NextDestination,
        PrevDestination,
        NextCompletion,
        PrevCompletion,
        Dismiss,
        PageUp,
        PageDown
    ]
);

const CONTEXT: &str = "Composer";
const MAX_COMPLETIONS: usize = 6;
const MAX_EMOTES: usize = 8;

/// A row in the popup above the composer.
#[derive(Clone)]
pub(crate) enum Suggestion {
    Command(&'static CommandSpec),
    /// With the platforms it works on.
    Emote(Vec<Platform>, EmoteOption),
}

/// The `:name` being typed just before the cursor: its byte range and the name so far.
pub(crate) fn emote_token(text: &str, cursor: usize) -> Option<(Range<usize>, &str)> {
    let cursor = (0..=cursor.min(text.len()))
        .rev()
        .find(|&i| text.is_char_boundary(i))?;
    let before = &text[..cursor];
    let start = before
        .char_indices()
        .rev()
        .find(|(_, c)| c.is_whitespace())
        .map_or(0, |(i, c)| i + c.len_utf8());
    let name = before[start..].strip_prefix(':')?;
    (!name.is_empty() && !name.contains(':')).then_some((start..cursor, name))
}

/// The text with `range` replaced by the emote and a space, and the cursor's new line and column.
pub(crate) fn insert_emote(text: &str, range: Range<usize>, name: &str) -> (String, Position) {
    let rest = &text[range.end..];
    let gap = if rest.starts_with(' ') { "" } else { " " };
    let head = format!("{}{name}{gap}", &text[..range.start]);
    let end = position_at(&head);
    // Past the space that was already there.
    let cursor = Position::new(end.line, end.character + u32::from(gap.is_empty()));
    (format!("{head}{rest}"), cursor)
}

/// The line and column (in characters) at the end of `head`.
fn position_at(head: &str) -> Position {
    let line = head.matches('\n').count() as u32;
    let column = head.rsplit('\n').next().unwrap_or("").chars().count() as u32;
    Position::new(line, column)
}

/// The plain word just before the cursor, for Tab completion: not a `:name`, `/command` or
/// `@mention`, and with nothing but space after the cursor.
pub(crate) fn tab_word(text: &str, cursor: usize) -> Option<(usize, &str)> {
    if !text.is_char_boundary(cursor.min(text.len())) {
        return None;
    }
    let cursor = cursor.min(text.len());
    if text[cursor..]
        .chars()
        .next()
        .is_some_and(|c| !c.is_whitespace())
    {
        return None;
    }
    let before = &text[..cursor];
    let start = before
        .char_indices()
        .rev()
        .find(|(_, c)| c.is_whitespace())
        .map_or(0, |(i, c)| i + c.len_utf8());
    let word = &before[start..];
    let plain = !word.is_empty() && !word.starts_with([':', '/', '@']);
    plain.then_some((start, word))
}

/// Emote names in Tab order: alphabetical, ignoring case.
pub(crate) fn tab_order(mut names: Vec<String>) -> Vec<String> {
    names.sort_by(|a, b| {
        a.to_lowercase()
            .cmp(&b.to_lowercase())
            .then_with(|| a.cmp(b))
    });
    names
}

/// Tab stepping through the emotes that start with a typed word.
pub(crate) struct EmoteCycle {
    names: Vec<String>,
    ix: usize,
    /// Where the word starts, in bytes.
    start: usize,
    /// The text and cursor right after the last step; anything else typed ends the cycle.
    text: String,
    cursor: usize,
}

/// Binds the composer's keys inside its text box.
pub fn init(cx: &mut App) {
    let input = Some("Composer > Input");
    cx.bind_keys([
        KeyBinding::new("tab", NextDestination, input),
        KeyBinding::new("shift-tab", PrevDestination, input),
        KeyBinding::new("down", NextCompletion, input),
        KeyBinding::new("up", PrevCompletion, input),
        KeyBinding::new("escape", Dismiss, input),
        KeyBinding::new("pageup", PageUp, input),
        KeyBinding::new("pagedown", PageDown, input),
    ]);
}

impl ChatView {
    /// Commands matching the composer text, or emotes for a `:name` before the cursor, unless
    /// dismissed with Esc.
    fn completions(&self, cx: &App) -> Vec<Suggestion> {
        let state = self.input.read(cx);
        let text = state.value();
        if self.completions_dismissed.as_ref() == Some(&text) {
            return Vec::new();
        }
        let mut commands = command::completions(&text);
        if !commands.is_empty() {
            commands.truncate(MAX_COMPLETIONS);
            return commands.into_iter().map(Suggestion::Command).collect();
        }
        match emote_token(&text, state.cursor()) {
            Some((_, name)) => self
                .session
                .emote_choices(self.destination, name, MAX_EMOTES)
                .into_iter()
                .map(|(p, e)| Suggestion::Emote(p, e))
                .collect(),
            None => Vec::new(),
        }
    }

    fn selected_completion(&self, cx: &App) -> Option<Suggestion> {
        let mut c = self.completions(cx);
        let ix = self.completion_ix.min(c.len().saturating_sub(1));
        (ix < c.len()).then(|| c.swap_remove(ix))
    }

    fn accept_completion(
        &mut self,
        suggestion: &Suggestion,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (text, cursor) = match suggestion {
            Suggestion::Command(spec) => {
                let text = format!("/{} ", spec.name);
                let len = text.chars().count() as u32;
                (text, Position::new(0, len))
            }
            Suggestion::Emote(_, emote) => {
                let state = self.input.read(cx);
                let text = state.value();
                let Some((range, _)) = emote_token(&text, state.cursor()) else {
                    return;
                };
                insert_emote(&text, range, &emote.name)
            }
        };
        self.input.update(cx, |state, cx| {
            state.set_value(text, window, cx);
            state.set_cursor_position(cursor, window, cx);
        });
        self.completion_ix = 0;
        cx.notify();
    }

    fn next_destination(
        &mut self,
        _: &NextDestination,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.tab_leaves {
            self.leave_composer(true, window, cx);
            return;
        }
        if let Some(suggestion) = self.selected_completion(cx) {
            self.accept_completion(&suggestion, window, cx);
            return;
        }
        if self.cycle_emote(1, window, cx) {
            return;
        }
        self.destination = self.destination.next(&self.session.sendable());
        cx.notify();
    }

    fn prev_destination(
        &mut self,
        _: &PrevDestination,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.tab_leaves {
            self.leave_composer(false, window, cx);
            return;
        }
        if !self.completions(cx).is_empty() {
            self.move_completion(-1, cx);
            return;
        }
        if self.cycle_emote(-1, window, cx) {
            return;
        }
        self.destination = self.destination.prev(&self.session.sendable());
        cx.notify();
    }

    /// Tab on a word: puts in the next (or previous) emote starting with it, alphabetically.
    /// Returns false when there's no word or no emote to put in.
    fn cycle_emote(&mut self, by: isize, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let (text, cursor) = {
            let state = self.input.read(cx);
            (state.value().to_string(), state.cursor())
        };
        let going = self
            .emote_cycle
            .as_ref()
            .is_some_and(|c| c.text == text && c.cursor == cursor);
        if !going {
            self.emote_cycle = None;
            let Some((start, word)) = tab_word(&text, cursor) else {
                return false;
            };
            let names = self
                .session
                .emote_choices(self.destination, word, usize::MAX)
                .into_iter()
                .map(|(_, e)| e.name)
                .collect::<Vec<_>>();
            if names.is_empty() {
                return false;
            }
            let names = tab_order(names);
            // The first step lands on the first name (or the last, going back).
            let ix = if by < 0 { 0 } else { names.len() - 1 };
            self.emote_cycle = Some(EmoteCycle {
                names,
                ix,
                start,
                text: String::new(),
                cursor,
            });
        }
        let Some(c) = self.emote_cycle.as_mut() else {
            return false;
        };
        c.ix = (c.ix as isize + by).rem_euclid(c.names.len() as isize) as usize;
        let name = &c.names[c.ix];
        let head = format!("{}{name}", &text[..c.start]);
        let new_text = format!("{head}{}", &text[cursor..]);
        c.cursor = head.len();
        c.text = new_text.clone();
        let at = position_at(&head);
        self.input.update(cx, |state, cx| {
            state.set_value(new_text, window, cx);
            state.set_cursor_position(at, window, cx);
        });
        cx.notify();
        true
    }

    fn next_completion(&mut self, _: &NextCompletion, _: &mut Window, cx: &mut Context<Self>) {
        if self.completions(cx).is_empty() {
            cx.propagate();
            return;
        }
        self.move_completion(1, cx);
    }

    fn prev_completion(&mut self, _: &PrevCompletion, _: &mut Window, cx: &mut Context<Self>) {
        if self.completions(cx).is_empty() {
            cx.propagate();
            return;
        }
        self.move_completion(-1, cx);
    }

    fn move_completion(&mut self, by: isize, cx: &mut Context<Self>) {
        let n = self.completions(cx).len() as isize;
        if n > 0 {
            self.completion_ix = (self.completion_ix as isize + by).rem_euclid(n) as usize;
            cx.notify();
        }
    }

    fn dismiss(&mut self, _: &Dismiss, _: &mut Window, cx: &mut Context<Self>) {
        if !self.completions(cx).is_empty() {
            self.completions_dismissed = Some(self.input.read(cx).value());
        } else if self.replying.is_some() {
            self.end_reply();
        } else if !matches!(self.panel, Panel::None) {
            self.panel = Panel::None;
        } else if !self.is_following() {
            self.jump_to_latest(cx);
            return;
        } else if self.composer_error.is_some() {
            self.composer_error = None;
        } else {
            cx.propagate();
            return;
        }
        cx.notify();
    }

    fn page_up(&mut self, _: &PageUp, _: &mut Window, cx: &mut Context<Self>) {
        self.scroll_feed_pages(-1.0, cx);
    }

    fn page_down(&mut self, _: &PageDown, _: &mut Window, cx: &mut Context<Self>) {
        self.scroll_feed_pages(1.0, cx);
    }

    /// Enter: accept a half-typed command, otherwise submit.
    pub(crate) fn on_enter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.input.read(cx).value();
        if text.trim().is_empty() && matches!(self.panel, Panel::Confirm { .. }) {
            self.confirm_batch(window, cx);
            return;
        }
        if let Some(suggestion) = self.selected_completion(cx) {
            let finished = match &suggestion {
                Suggestion::Command(spec) => text.trim() == format!("/{}", spec.name),
                Suggestion::Emote(..) => false,
            };
            if !finished {
                self.accept_completion(&suggestion, window, cx);
                return;
            }
        }
        let submitted = match &self.replying {
            Some(r) => self
                .session
                .submit_reply(&text, r.platform, &r.message_id, now_ms()),
            None => self.session.submit(&text, self.destination, now_ms()),
        };
        if self.handle_submitted(submitted, window, cx) {
            self.end_reply();
            self.input
                .update(cx, |state, cx| state.set_value("", window, cx));
        }
    }

    /// Starts a reply: the composer goes to the message's platform and quotes it.
    pub(crate) fn start_reply(
        &mut self,
        replying: Replying,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.replying.is_none() {
            self.before_reply = Some(self.destination);
        }
        self.destination = Destination::One(replying.platform);
        self.replying = Some(replying);
        self.panel = Panel::None;
        self.focus_composer(window, cx);
    }

    /// Stops replying and puts the destination back.
    fn end_reply(&mut self) {
        if self.replying.take().is_some()
            && let Some(d) = self.before_reply.take()
        {
            self.destination = d;
        }
    }

    /// Acts on a submit result. Returns whether the composer text was used up.
    pub(crate) fn handle_submitted(
        &mut self,
        submitted: Submitted,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let used = match submitted {
            Submitted::Nothing => false,
            Submitted::Send(out) => {
                self.dispatch(out);
                self.jump_to_latest(cx);
                true
            }
            Submitted::Local => true,
            Submitted::NeedsTarget {
                plan,
                prompt,
                choices,
            } => {
                self.panel = Panel::Target {
                    plan,
                    prompt,
                    choices,
                };
                true
            }
            Submitted::PollDialog(platform) => {
                self.open_poll(platform, window, cx);
                true
            }
            Submitted::Rejected(why) => {
                self.composer_error = Some(why.into());
                false
            }
            Submitted::Confirm { prompt, batch } => {
                self.panel = Panel::Confirm { prompt, batch };
                true
            }
            Submitted::Search(text) => {
                let text = (!text.trim().is_empty()).then_some(text);
                self.open_search(text, window, cx);
                true
            }
        };
        self.sync_list();
        cx.notify();
        used
    }

    pub(crate) fn render_composer(
        &mut self,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let connected = self.session.sendable();
        let (label, label_color) = match self.destination {
            _ if connected.is_empty() => {
                ("To: nothing connected yet".to_string(), rgb(style::FAINT))
            }
            Destination::All => ("To: All platforms".to_string(), rgb(style::TEXT)),
            Destination::One(p) => (format!("To: {p}"), style::platform_color(p)),
        };
        let icons: Vec<_> = self.destination.targets(&connected);
        // A count once a message nears the strictest target's limit.
        let limit = icons.iter().filter_map(|p| p.max_message_chars()).min();
        let typed = self.input.read(cx).value().chars().count();
        let count = limit
            .filter(|max| typed * 5 >= max * 4 && !self.input.read(cx).value().starts_with('/'))
            .map(|max| {
                div()
                    .text_color(rgb(if typed > max {
                        style::ERROR
                    } else {
                        style::WARN
                    }))
                    .child(format!("{typed}/{max}"))
            });
        let completions = self.completions(cx);
        let selected = self.completion_ix.min(completions.len().saturating_sub(1));
        let popup = (!completions.is_empty()).then(|| {
            div()
                .flex()
                .flex_col()
                .mb_1()
                .p_1()
                .rounded_md()
                .bg(rgb(style::PANEL))
                .border_1()
                .border_color(rgb(style::BORDER))
                .children(completions.into_iter().enumerate().map(|(i, suggestion)| {
                    let row = div()
                        .id(("suggestion", i))
                        .flex()
                        .items_center()
                        .gap_2()
                        .px_2()
                        .py_0p5()
                        .rounded_sm()
                        .cursor_pointer()
                        .when(i == selected, |d| d.bg(rgb(style::RAISED)))
                        .hover(|s| s.bg(rgb(style::RAISED)));
                    let row = match &suggestion {
                        Suggestion::Command(spec) => {
                            let kind = match spec.is_platform() {
                                false => "Rill",
                                true => "Platform",
                            };
                            row.child(div().flex_none().child(spec.usage))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .text_color(rgb(style::MUTED))
                                        .child(spec.summary),
                                )
                                .child(
                                    div()
                                        .flex_none()
                                        .text_xs()
                                        .text_color(rgb(style::FAINT))
                                        .child(kind),
                                )
                        }
                        Suggestion::Emote(platforms, emote) => row
                            .child(div().flex_none().w(px(36.)).flex().justify_center().child(
                                row::emote_image(&emote.name, emote.url.as_deref(), emote.size),
                            ))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .child(emote.name.clone()),
                            )
                            .children(platforms.iter().map(|p| style::platform_icon(*p, 12.))),
                    };
                    row.on_click(cx.listener(move |view, _, window, cx| {
                        view.accept_completion(&suggestion, window, cx);
                    }))
                }))
        });

        div()
            .key_context(CONTEXT)
            .on_action(cx.listener(Self::next_destination))
            .on_action(cx.listener(Self::prev_destination))
            .on_action(cx.listener(Self::next_completion))
            .on_action(cx.listener(Self::prev_completion))
            .on_action(cx.listener(Self::dismiss))
            .on_action(cx.listener(Self::page_up))
            .on_action(cx.listener(Self::page_down))
            .flex()
            .flex_col()
            .gap_1()
            .px_3()
            .pb_3()
            .pt_1()
            .children(popup)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .text_xs()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .text_color(label_color)
                            .child(label)
                            .children(icons.into_iter().map(|p| style::platform_icon(p, 12.))),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .text_color(rgb(style::FAINT))
                                    .child(if self.tab_leaves {
                                        "Tab leaves the composer · Esc stays"
                                    } else {
                                        "Tab switches · / commands · : emotes · PgUp scrolls"
                                    }),
                            )
                            .children(count),
                    ),
            )
            .child(
                outline(self.destination).child(
                    div().rounded_md().bg(rgb(style::PANEL)).child(
                        Textarea::new(&self.input)
                            .bordered(false)
                            .aria_label("Message"),
                    ),
                ),
            )
            .when_some(self.replying.clone(), |d, r| {
                let text: String = r.text.chars().take(80).collect();
                d.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .text_xs()
                        .child(style::platform_icon(r.platform, 12.))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_color(rgb(style::MUTED))
                                .child(format!("Replying to {}: {text}", r.author)),
                        )
                        .child(div().text_color(rgb(style::FAINT)).child("Esc cancels")),
                )
            })
            .when_some(self.composer_error.clone(), |d, why| {
                d.child(div().text_xs().text_color(rgb(style::ERROR)).child(why))
            })
    }
}

/// A 2px static outline: the platform's color, or a rainbow for ALL.
fn outline(destination: Destination) -> gpui::Div {
    let frame = div().relative().p(px(2.)).rounded_lg();
    match destination {
        Destination::One(p) => frame.bg(style::platform_color(p)),
        Destination::All => {
            let stops = style::RAINBOW;
            let last = stops.len() - 2;
            frame.child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .children((0..=last).map(|i| {
                        div()
                            .flex_1()
                            .h_full()
                            .bg(linear_gradient(
                                90.,
                                linear_color_stop(style::hsla(stops[i]), 0.),
                                linear_color_stop(style::hsla(stops[i + 1]), 1.),
                            ))
                            .when(i == 0, |d| d.rounded_l_lg())
                            .when(i == last, |d| d.rounded_r_lg())
                    })),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_emote_name_before_the_cursor() {
        assert_eq!(emote_token(":s", 2), Some((0..2, "s")));
        assert_eq!(emote_token("hi :smi", 7), Some((3..7, "smi")));
        assert_eq!(emote_token("hi :smi there", 7), Some((3..7, "smi")));
        assert_eq!(
            emote_token("héllo :Ka", "héllo :Ka".len()),
            Some((7..10, "Ka"))
        );
        assert_eq!(emote_token(":", 1), None, "a colon alone");
        assert_eq!(
            emote_token("at 12:30", 8),
            None,
            "not at the start of a word"
        );
        assert_eq!(emote_token("hi :smi there", 13), None, "cursor past it");
        assert_eq!(emote_token("::x", 3), None);
    }

    #[test]
    fn inserting_an_emote_replaces_the_name_and_adds_a_space() {
        let (text, at) = insert_emote("hi :smi", 3..7, "Smile");
        assert_eq!(text, "hi Smile ");
        assert_eq!(at, Position::new(0, 9));
        let (text, at) = insert_emote("hi :smi there", 3..7, "Smile");
        assert_eq!(text, "hi Smile there");
        assert_eq!(
            at,
            Position::new(0, 9),
            "after the space that was already there"
        );
        let (text, at) = insert_emote("é\nok :K", 6..8, "Kappa");
        assert_eq!(text, "é\nok Kappa ");
        assert_eq!(at, Position::new(1, 9));
    }

    #[test]
    fn tab_takes_the_plain_word_before_the_cursor() {
        assert_eq!(tab_word("s", 1), Some((0, "s")));
        assert_eq!(tab_word("hi Sm", 5), Some((3, "Sm")));
        assert_eq!(tab_word("hi sm there", 5), Some((3, "sm")));
        assert_eq!(tab_word("hi smthere", 5), None, "in the middle of a word");
        assert_eq!(tab_word("hi ", 3), None, "no word");
        assert_eq!(tab_word("", 0), None);
        assert_eq!(tab_word("hi :sm", 6), None, "the : list handles it");
        assert_eq!(tab_word("/ban", 4), None);
        assert_eq!(tab_word("hi @bo", 6), None);
        assert_eq!(tab_word("é s", "é s".len()), Some((3, "s")));
    }

    #[test]
    fn tab_order_is_alphabetical_ignoring_case() {
        let names = ["smile", "Smash", "sadge", "SADGE"]
            .map(String::from)
            .to_vec();
        assert_eq!(tab_order(names), ["SADGE", "sadge", "Smash", "smile"]);
    }
}
