//! The composer: destination outline and label, Tab cycling, `/` autocomplete and submitting.

use gpui::{
    App, Context, InteractiveElement, IntoElement, KeyBinding, ParentElement,
    StatefulInteractiveElement, Styled, Window, actions, div, linear_color_stop, linear_gradient,
    prelude::FluentBuilder, px, rgb,
};
use gpui_component::input::{Position, Textarea};
use rill_core::command::{self, CommandSpec};
use rill_core::{Destination, Submitted, now_ms};

use crate::chat_view::ChatView;
use crate::panels::Panel;
use crate::style;

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
    /// Commands matching the composer text, unless dismissed with Esc.
    fn completions(&self, cx: &App) -> Vec<&'static CommandSpec> {
        let text = self.input.read(cx).value();
        if self.completions_dismissed.as_ref() == Some(&text) {
            return Vec::new();
        }
        let mut c = command::completions(&text);
        c.truncate(MAX_COMPLETIONS);
        c
    }

    fn selected_completion(&self, cx: &App) -> Option<&'static CommandSpec> {
        let c = self.completions(cx);
        c.get(self.completion_ix.min(c.len().saturating_sub(1)))
            .copied()
    }

    fn accept_completion(
        &mut self,
        spec: &CommandSpec,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let text = format!("/{} ", spec.name);
        let len = text.len() as u32;
        self.input.update(cx, |state, cx| {
            state.set_value(text, window, cx);
            state.set_cursor_position(Position::new(0, len), window, cx);
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
        if let Some(spec) = self.selected_completion(cx) {
            self.accept_completion(spec, window, cx);
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
        self.destination = self.destination.prev(&self.session.sendable());
        cx.notify();
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
        if let Some(spec) = self.selected_completion(cx)
            && text.trim() != format!("/{}", spec.name)
        {
            self.accept_completion(spec, window, cx);
            return;
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
                .children(completions.iter().enumerate().map(|(i, spec)| {
                    let spec: &'static CommandSpec = spec;
                    let kind = match spec.is_platform() {
                        false => "Rill",
                        true => "Platform",
                    };
                    div()
                        .id(spec.name)
                        .flex()
                        .gap_2()
                        .px_2()
                        .py_0p5()
                        .rounded_sm()
                        .cursor_pointer()
                        .when(i == selected, |d| d.bg(rgb(style::RAISED)))
                        .hover(|s| s.bg(rgb(style::RAISED)))
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
                        .on_click(cx.listener(move |view, _, window, cx| {
                            view.accept_completion(spec, window, cx);
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
                                        "Tab switches · / commands · PgUp scrolls"
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
