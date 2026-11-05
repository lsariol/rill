//! The chat window spike: sent messages above, the destination-aware composer below.

use gpui::{
    App, AppContext, Context, Entity, InteractiveElement, IntoElement, KeyBinding, ParentElement,
    Render, SharedString, Styled, Subscription, Window, actions, div, linear_color_stop,
    linear_gradient, prelude::FluentBuilder, px, rgb,
};
use gpui_component::input::{InputEvent, Textarea, TextareaState};
use rill_core::{Destination, Platform};

use crate::style;

actions!(composer, [NextDestination, PrevDestination]);

const CONTEXT: &str = "Composer";

/// Binds Tab and Shift+Tab inside the composer's text box to destination cycling.
pub fn init(cx: &mut App) {
    let input = Some("Composer > Input");
    cx.bind_keys([
        KeyBinding::new("tab", NextDestination, input),
        KeyBinding::new("shift-tab", PrevDestination, input),
    ]);
}

/// The spike's only window.
pub struct ChatWindow {
    input: Entity<TextareaState>,
    destination: Destination,
    connected: Vec<Platform>,
    sent: Vec<(Vec<Platform>, SharedString)>,
    _subscriptions: Vec<Subscription>,
}

impl ChatWindow {
    /// Creates the window with every platform shown as connected.
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, 6)
                .submit_on_enter(true)
                .placeholder("Send a message")
        });
        let subscription = cx.subscribe_in(&input, window, Self::on_input);
        input.update(cx, |state, cx| state.focus(window, cx));
        Self {
            input,
            destination: Destination::All,
            connected: Platform::ALL.to_vec(),
            sent: Vec::new(),
            _subscriptions: vec![subscription],
        }
    }

    fn on_input(
        &mut self,
        input: &Entity<TextareaState>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let InputEvent::PressEnter { shift: false, .. } = event else {
            return;
        };
        let text = input.read(cx).value();
        if text.trim().is_empty() {
            return;
        }
        let targets = self.destination.targets(&self.connected);
        self.sent.push((targets, text));
        input.update(cx, |state, cx| state.set_value("", window, cx));
        cx.notify();
    }

    fn next_destination(&mut self, _: &NextDestination, _: &mut Window, cx: &mut Context<Self>) {
        self.destination = self.destination.next(&self.connected);
        cx.notify();
    }

    fn prev_destination(&mut self, _: &PrevDestination, _: &mut Window, cx: &mut Context<Self>) {
        self.destination = self.destination.prev(&self.connected);
        cx.notify();
    }

    fn label(&self) -> SharedString {
        match self.destination {
            Destination::All => "To: All platforms".into(),
            Destination::One(p) => format!("To: {p}").into(),
        }
    }
}

impl Render for ChatWindow {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let messages = div()
            .flex_1()
            .flex()
            .flex_col()
            .justify_end()
            .gap_1()
            .p_3()
            .children(self.sent.iter().map(|(targets, text)| {
                div()
                    .flex()
                    .gap_2()
                    .children(targets.iter().map(|p| {
                        div()
                            .text_xs()
                            .text_color(style::platform_color(*p))
                            .child(p.name())
                    }))
                    .child(div().text_color(rgb(style::TEXT)).child(text.clone()))
            }));

        let label_color = match self.destination {
            Destination::All => rgb(style::TEXT),
            Destination::One(p) => style::platform_color(p),
        };

        let composer = div()
            .key_context(CONTEXT)
            .on_action(cx.listener(Self::next_destination))
            .on_action(cx.listener(Self::prev_destination))
            .flex()
            .flex_col()
            .gap_1()
            .p_3()
            .child(
                div()
                    .flex()
                    .justify_between()
                    .text_xs()
                    .child(div().text_color(label_color).child(self.label()))
                    .child(div().text_color(rgb(style::MUTED)).child("Tab to switch")),
            )
            .child(
                outline(self.destination).child(
                    div().rounded_md().bg(rgb(style::PANEL)).child(
                        Textarea::new(&self.input)
                            .bordered(false)
                            .aria_label("Message"),
                    ),
                ),
            );

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(style::BACKGROUND))
            .child(messages)
            .child(composer)
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
