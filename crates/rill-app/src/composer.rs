//! The composer: destination, Tab cycling, autocomplete and submitting.

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
/// Emote or name rows shown at once; the rest scroll.
const VISIBLE_ROWS: usize = 8;

/// A row in the popup above the composer.
#[derive(Clone)]
pub(crate) enum Suggestion {
    Command(&'static CommandSpec),
    /// With the platforms it works on.
    Emote(Vec<Platform>, EmoteOption),
    /// Someone seen in chat, with the platforms they were seen on.
    User(Vec<Platform>, String),
}

/// The `:name` being typed before the cursor: its byte range and the name so far.
pub(crate) fn emote_token(text: &str, cursor: usize) -> Option<(Range<usize>, &str)> {
    token(text, cursor, ':')
}

/// The `@name` being typed just before the cursor, like [`emote_token`].
pub(crate) fn mention_token(text: &str, cursor: usize) -> Option<(Range<usize>, &str)> {
    token(text, cursor, '@')
}

/// The word before the cursor when it starts with `sigil`: its byte range and the rest.
fn token(text: &str, cursor: usize, sigil: char) -> Option<(Range<usize>, &str)> {
    let cursor = (0..=cursor.min(text.len()))
        .rev()
        .find(|&i| text.is_char_boundary(i))?;
    let before = &text[..cursor];
    let start = before
        .char_indices()
        .rev()
        .find(|(_, c)| c.is_whitespace())
        .map_or(0, |(i, c)| i + c.len_utf8());
    let name = before[start..].strip_prefix(sigil)?;
    (!name.contains(sigil)).then_some((start..cursor, name))
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

/// The first row to show so that `selected` is in view, moving `top` as little as possible.
fn in_view(top: usize, selected: usize, total: usize) -> usize {
    let top = if selected < top {
        selected
    } else if selected >= top + VISIBLE_ROWS {
        selected + 1 - VISIBLE_ROWS
    } else {
        top
    };
    top.min(total.saturating_sub(VISIBLE_ROWS))
}

/// The line and column (in characters) at the end of `head`.
fn position_at(head: &str) -> Position {
    let line = head.matches('\n').count() as u32;
    let column = head.rsplit('\n').next().unwrap_or("").chars().count() as u32;
    Position::new(line, column)
}

/// The plain word before the cursor for Tab completion, if only space follows the cursor.
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

/// Emote and user names in Tab order: alphabetical, ignoring case, each once.
pub(crate) fn tab_order(mut names: Vec<String>) -> Vec<String> {
    names.sort_by(|a, b| {
        a.to_lowercase()
            .cmp(&b.to_lowercase())
            .then_with(|| a.cmp(b))
    });
    names.dedup();
    names
}

/// Tab stepping through the emotes and names that start with a typed word.
pub(crate) struct TabCycle {
    names: Vec<String>,
    ix: usize,
    /// Where the word starts, in bytes.
    start: usize,
    /// The text and cursor right after the last step; anything else typed ends the cycle.
    text: String,
    cursor: usize,
}

/// Binds the composer's keys inside its text box.
/// Messages sent from the composer, oldest first, for stepping through with Up and Down.
#[derive(Debug, Default)]
pub(crate) struct SentHistory {
    sent: Vec<String>,
    /// The message shown while stepping through.
    at: Option<usize>,
    /// What was typed before stepping back, brought back after the newest.
    draft: String,
}

impl SentHistory {
    /// The most messages kept.
    const MAX: usize = 100;

    /// Adds a sent message, once if it repeats the last one, and ends any stepping through.
    pub(crate) fn push(&mut self, text: &str) {
        let text = text.trim_end();
        if !text.is_empty() && self.sent.last().map(String::as_str) != Some(text) {
            self.sent.push(text.to_string());
            if self.sent.len() > Self::MAX {
                self.sent.remove(0);
            }
        }
        self.at = None;
        self.draft.clear();
    }

    /// Up: the message before the one shown; `None` at the oldest.
    pub(crate) fn older(&mut self, typed: &str) -> Option<&str> {
        let i = match self.at {
            None => {
                let newest = self.sent.len().checked_sub(1)?;
                self.draft = typed.to_string();
                newest
            }
            Some(0) => return None,
            Some(i) => i - 1,
        };
        self.at = Some(i);
        Some(&self.sent[i])
    }

    /// Down: the message after the one shown, then what was typed; `None` when not stepping.
    pub(crate) fn newer(&mut self) -> Option<String> {
        let i = self.at?;
        if i + 1 < self.sent.len() {
            self.at = Some(i + 1);
            Some(self.sent[i + 1].clone())
        } else {
            self.at = None;
            Some(std::mem::take(&mut self.draft))
        }
    }
}

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
    /// Commands, emotes or people matching the composer text, unless dismissed with Esc.
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
        let cursor = state.cursor();
        if let Some((_, name)) = emote_token(&text, cursor) {
            return self
                .session
                .emote_choices(self.destination, name, usize::MAX)
                .into_iter()
                .map(|(p, e)| Suggestion::Emote(p, e))
                .collect();
        }
        if let Some((_, name)) = mention_token(&text, cursor) {
            return self
                .session
                .user_choices(self.destination, name, usize::MAX)
                .into_iter()
                .map(|(p, n)| Suggestion::User(p, n))
                .collect();
        }
        Vec::new()
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
            Suggestion::User(_, name) => {
                let state = self.input.read(cx);
                let text = state.value();
                let Some((range, _)) = mention_token(&text, state.cursor()) else {
                    return;
                };
                insert_emote(&text, range, &format!("@{name}"))
            }
        };
        self.input.update(cx, |state, cx| {
            state.set_value(text, window, cx);
            state.set_cursor_position(cursor, window, cx);
        });
        self.completion_ix = 0;
        self.completion_top = 0;
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
        if self.cycle_word(1, window, cx) {
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
        if self.cycle_word(-1, window, cx) {
            return;
        }
        self.destination = self.destination.prev(&self.session.sendable());
        cx.notify();
    }

    /// Tab on a word: puts in the next or previous matching emote or name; false if none.
    fn cycle_word(&mut self, by: isize, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let (text, cursor) = {
            let state = self.input.read(cx);
            (state.value().to_string(), state.cursor())
        };
        let going = self
            .tab_cycle
            .as_ref()
            .is_some_and(|c| c.text == text && c.cursor == cursor);
        if !going {
            self.tab_cycle = None;
            let Some((start, word)) = tab_word(&text, cursor) else {
                return false;
            };
            let emotes = self
                .session
                .emote_choices(self.destination, word, usize::MAX)
                .into_iter()
                .map(|(_, e)| e.name);
            let users = self
                .session
                .user_choices(self.destination, word, usize::MAX)
                .into_iter()
                .map(|(_, n)| n);
            let names = emotes.chain(users).collect::<Vec<_>>();
            if names.is_empty() {
                return false;
            }
            let names = tab_order(names);
            // The first step lands on the first name (or the last, going back).
            let ix = if by < 0 { 0 } else { names.len() - 1 };
            self.tab_cycle = Some(TabCycle {
                names,
                ix,
                start,
                text: String::new(),
                cursor,
            });
        }
        let Some(c) = self.tab_cycle.as_mut() else {
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

    fn next_completion(&mut self, _: &NextCompletion, window: &mut Window, cx: &mut Context<Self>) {
        if self.completions(cx).is_empty() {
            // Down on the last line steps forward through sent messages.
            let (text, cursor) = {
                let input = self.input.read(cx);
                (input.value(), input.cursor())
            };
            if !text.get(cursor..).unwrap_or_default().contains('\n')
                && let Some(newer) = self.sent_history.newer()
            {
                self.show_in_composer(&newer, window, cx);
                return;
            }
            cx.propagate();
            return;
        }
        self.move_completion(1, cx);
    }

    fn prev_completion(&mut self, _: &PrevCompletion, window: &mut Window, cx: &mut Context<Self>) {
        if self.completions(cx).is_empty() {
            // Up on the first line steps back through sent messages, as in Chatterino.
            let (text, cursor) = {
                let input = self.input.read(cx);
                (input.value(), input.cursor())
            };
            if !text.get(..cursor).unwrap_or_default().contains('\n')
                && let Some(older) = self.sent_history.older(&text).map(str::to_string)
            {
                self.show_in_composer(&older, window, cx);
                return;
            }
            cx.propagate();
            return;
        }
        self.move_completion(-1, cx);
    }

    /// Fills the composer with `text`, the cursor at its end.
    fn show_in_composer(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let end = position_at(text);
        self.input.update(cx, |state, cx| {
            state.set_value(text.to_string(), window, cx);
            state.set_cursor_position(end, window, cx);
        });
        cx.notify();
    }

    fn move_completion(&mut self, by: isize, cx: &mut Context<Self>) {
        let n = self.completions(cx).len() as isize;
        if n > 0 {
            self.completion_ix = (self.completion_ix as isize + by).rem_euclid(n) as usize;
            self.completion_top = in_view(self.completion_top, self.completion_ix, n as usize);
            cx.notify();
        }
    }

    /// Scrolls the list without moving the selection.
    fn scroll_completions(&mut self, by: isize, cx: &mut Context<Self>) {
        let n = self.completions(cx).len();
        let max = n.saturating_sub(VISIBLE_ROWS) as isize;
        self.completion_top = (self.completion_top as isize + by).clamp(0, max) as usize;
        cx.notify();
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
        if self.completions(cx).len() > VISIBLE_ROWS {
            self.page_completions(-1, cx);
        } else {
            self.scroll_feed_pages(-1.0, cx);
        }
    }

    fn page_down(&mut self, _: &PageDown, _: &mut Window, cx: &mut Context<Self>) {
        if self.completions(cx).len() > VISIBLE_ROWS {
            self.page_completions(1, cx);
        } else {
            self.scroll_feed_pages(1.0, cx);
        }
    }

    /// PgUp and PgDn in a long list: a page at a time, stopping at the ends.
    fn page_completions(&mut self, by: isize, cx: &mut Context<Self>) {
        let n = self.completions(cx).len();
        let page = VISIBLE_ROWS as isize - 1;
        self.completion_ix =
            (self.completion_ix as isize + by * page).clamp(0, n as isize - 1) as usize;
        self.completion_top = in_view(self.completion_top, self.completion_ix, n);
        cx.notify();
    }

    /// Enter: accept a half-typed command, otherwise submit.
    pub(crate) fn on_enter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.input.read(cx).value();
        if text.trim().is_empty() && matches!(self.panel, Panel::Confirm { .. }) {
            self.confirm_batch(window, cx);
            return;
        }
        if let Some(suggestion) = self.selected_completion(cx) {
            // A name typed out in full sends, rather than being filled in again.
            let cursor = self.input.read(cx).cursor();
            let finished = match &suggestion {
                Suggestion::Command(spec) => text.trim() == format!("/{}", spec.name),
                Suggestion::Emote(_, e) => {
                    emote_token(&text, cursor).is_some_and(|(_, typed)| typed == e.name)
                }
                Suggestion::User(_, n) => mention_token(&text, cursor)
                    .is_some_and(|(_, typed)| typed.eq_ignore_ascii_case(n)),
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
            self.sent_history.push(&text);
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
                self.target_ix = 0;
                window.focus(&self.target_focus, cx);
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
        let total = completions.len();
        let selected = self.completion_ix.min(total.saturating_sub(1));
        let top = self.completion_top.min(total.saturating_sub(VISIBLE_ROWS));
        let more = (total > VISIBLE_ROWS).then(|| {
            div()
                .px_2()
                .text_xs()
                .text_color(rgb(style::FAINT))
                .child(format!(
                    "{}–{} of {total} · PgUp and PgDn page",
                    top + 1,
                    (top + VISIBLE_ROWS).min(total)
                ))
        });
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
                .id("suggestions")
                .on_scroll_wheel(cx.listener(|view, e: &gpui::ScrollWheelEvent, window, cx| {
                    let line = window.line_height();
                    let dy = f32::from(e.delta.pixel_delta(line).y) / f32::from(line);
                    let rows = if dy.abs() < 1. {
                        dy.signum()
                    } else {
                        dy.round()
                    };
                    view.scroll_completions(-(rows as isize), cx);
                }))
                .children(
                    completions
                        .into_iter()
                        .enumerate()
                        .skip(top)
                        .take(VISIBLE_ROWS)
                        .map(|(i, suggestion)| {
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
                                    // The summary is cut short in a narrow window; hovering shows it whole.
                                    let mut tip = format!("{}\n{}", spec.usage, spec.summary);
                                    if !spec.aliases.is_empty() {
                                        let names: Vec<_> =
                                            spec.aliases.iter().map(|a| format!("/{a}")).collect();
                                        tip.push_str(&format!("\nAlso {}", names.join(", ")));
                                    }
                                    row.tooltip(move |window, cx| {
                                        gpui_component::tooltip::Tooltip::new(tip.clone())
                                            .build(window, cx)
                                    })
                                    .child(div().flex_none().child(spec.usage))
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
                                    .child(
                                        div().flex_none().w(px(36.)).flex().justify_center().child(
                                            row::emote_image(
                                                &emote.name,
                                                emote.url.as_deref(),
                                                emote.size,
                                            ),
                                        ),
                                    )
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .truncate()
                                            .child(emote.name.clone()),
                                    )
                                    .children(
                                        platforms.iter().map(|p| style::platform_icon(*p, 12.)),
                                    ),
                                Suggestion::User(platforms, name) => row
                                    .child(
                                        div()
                                            .flex_none()
                                            .w(px(36.))
                                            .flex()
                                            .justify_center()
                                            .text_color(rgb(style::FAINT))
                                            .child("@"),
                                    )
                                    .child(div().flex_1().min_w_0().truncate().child(name.clone()))
                                    .children(
                                        platforms.iter().map(|p| style::platform_icon(*p, 12.)),
                                    ),
                            };
                            row.on_click(cx.listener(move |view, _, window, cx| {
                                view.accept_completion(&suggestion, window, cx);
                            }))
                        }),
                )
                .children(more)
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
                                        "Tab switches · / commands · : emotes · @ names · PgUp scrolls"
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
        assert_eq!(
            emote_token(":", 1),
            Some((0..1, "")),
            "a colon alone lists them all"
        );
        assert_eq!(emote_token("hi :", 4), Some((3..4, "")));
        assert_eq!(
            emote_token("at 12:30", 8),
            None,
            "not at the start of a word"
        );
        assert_eq!(emote_token("hi :smi there", 13), None, "cursor past it");
        assert_eq!(emote_token("::x", 3), None);
    }

    #[test]
    fn finds_the_mention_before_the_cursor() {
        assert_eq!(mention_token("hi @mo", 6), Some((3..6, "mo")));
        assert_eq!(
            mention_token("@", 1),
            Some((0..1, "")),
            "an @ alone lists everyone"
        );
        assert_eq!(
            mention_token("a@b.com", 7),
            None,
            "not at the start of a word"
        );
        assert_eq!(mention_token("hi :mo", 6), None);
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
    fn the_list_scrolls_only_as_far_as_the_selection_needs() {
        assert_eq!(in_view(0, 3, 100), 0);
        assert_eq!(in_view(0, 8, 100), 1, "one past the bottom");
        assert_eq!(in_view(10, 4, 100), 4, "above the top");
        assert_eq!(in_view(0, 99, 100), 92);
        assert_eq!(in_view(50, 2, 5), 0, "a short list never scrolls");
    }

    #[test]
    fn up_and_down_step_through_sent_messages_and_back_to_the_draft() {
        let mut h = SentHistory::default();
        assert_eq!(h.older(""), None, "nothing sent yet");
        h.push("first");
        h.push("second");
        h.push("second");
        h.push("/ban spammer");
        assert_eq!(h.older("half typed"), Some("/ban spammer"));
        assert_eq!(h.older("/ban spammer"), Some("second"));
        assert_eq!(h.older("second"), Some("first"), "a repeat is kept once");
        assert_eq!(h.older("first"), None, "the oldest stays");
        assert_eq!(h.newer().as_deref(), Some("second"));
        assert_eq!(h.newer().as_deref(), Some("/ban spammer"));
        assert_eq!(
            h.newer().as_deref(),
            Some("half typed"),
            "the draft comes back"
        );
        assert_eq!(h.newer(), None, "Down does nothing once back");
        h.older("");
        h.push("third");
        assert_eq!(
            h.older(""),
            Some("third"),
            "sending starts again from the newest"
        );
    }

    #[test]
    fn tab_order_is_alphabetical_ignoring_case() {
        let names = ["smile", "Smash", "sadge", "SADGE"]
            .map(String::from)
            .to_vec();
        assert_eq!(tab_order(names), ["SADGE", "sadge", "Smash", "smile"]);
        let names = ["mobasity", "Moe", "mobasity"].map(String::from).to_vec();
        assert_eq!(
            tab_order(names),
            ["mobasity", "Moe"],
            "an emote and a name alike, once"
        );
    }
}
