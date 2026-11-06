//! Ctrl+F: a search bar that shows only the messages matching what's typed.

use gpui::{
    App, AppContext, Context, Entity, IntoElement, KeyBinding, ParentElement, Styled, Subscription,
    Window, actions, div, prelude::FluentBuilder, rgb,
};
use gpui_component::Sizable;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputEvent, InputState};

use crate::chat_view::ChatView;
use crate::{focus, style};

actions!(search, [Find]);

/// An open search bar.
pub struct Search {
    pub(crate) input: Entity<InputState>,
    _changes: Subscription,
}

/// Binds Ctrl+F (Cmd+F on macOS) anywhere in the window.
pub fn init(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("secondary-f", Find, Some(focus::CONTEXT))]);
}

impl ChatView {
    /// Opens the search bar, or selects its text when it's already open.
    pub(crate) fn on_find(&mut self, _: &Find, window: &mut Window, cx: &mut Context<Self>) {
        self.open_search(None, window, cx);
    }

    /// Opens the search bar (with `text`, from `/search`), or selects its text when it's open.
    pub(crate) fn open_search(
        &mut self,
        text: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(text) = &text {
            if self.search.is_none() {
                self.open_search(None, window, cx);
            }
            if let Some(search) = &self.search {
                // Setting the value fires a change, which applies the filter.
                search
                    .input
                    .update(cx, |s, cx| s.set_value(text.clone(), window, cx));
            }
            let mut filter = self.session.feed.filter().clone();
            filter.search_for(text);
            self.set_filter(filter, cx);
            return;
        }
        if let Some(search) = &self.search {
            search.input.update(cx, |s, cx| {
                s.focus(window, cx);
                s.select_all(window, cx);
            });
            return;
        }
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Search messages"));
        let changes = cx.subscribe(&input, |view, input, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                let text = input.read(cx).value();
                let mut filter = view.session.feed.filter().clone();
                filter.search_for(&text);
                view.set_filter(filter, cx);
            }
        });
        input.update(cx, |s, cx| s.focus(window, cx));
        self.search = Some(Search {
            input,
            _changes: changes,
        });
        cx.notify();
    }

    /// Closes the search bar and shows every message again.
    pub(crate) fn close_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.search.take().is_none() {
            return;
        }
        let mut filter = self.session.feed.filter().clone();
        filter.search = None;
        self.set_filter(filter, cx);
        self.focus_composer(window, cx);
    }

    pub(crate) fn search_focused(&self, window: &Window, cx: &App) -> bool {
        self.search.as_ref().is_some_and(|s| {
            gpui::Focusable::focus_handle(s.input.read(cx), cx).contains_focused(window, cx)
        })
    }

    pub(crate) fn render_search(&mut self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let search = self.search.as_ref()?;
        let searching = self.session.feed.filter().search.is_some();
        let matches = self.session.feed.visible_len();
        let count = match (searching, matches) {
            (false, _) => String::new(),
            (true, 1) => "1 match".into(),
            (true, n) => format!("{n} matches"),
        };
        Some(
            div()
                .flex()
                .items_center()
                .gap_2()
                .px_2()
                .py_1()
                .border_b_1()
                .border_color(rgb(style::BORDER))
                .bg(rgb(style::RAISED))
                .child(
                    div()
                        .flex_1()
                        .child(Input::new(&search.input).small().cleanable(true)),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(style::MUTED))
                        .when(searching && matches == 0, |d| {
                            d.text_color(rgb(style::FAINT))
                        })
                        .child(if searching && matches == 0 {
                            "No matches".to_string()
                        } else {
                            count
                        }),
                )
                .child(
                    Button::new("close-search")
                        .ghost()
                        .xsmall()
                        .label("✕")
                        .accessibility_label("Close search")
                        .tooltip("Close search (Esc)")
                        .on_click(cx.listener(|view, _, window, cx| view.close_search(window, cx))),
                ),
        )
    }
}
