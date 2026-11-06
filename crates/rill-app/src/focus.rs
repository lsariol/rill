//! Keyboard focus: Tab moves between controls, F6 jumps between the composer and the rest of the
//! window, Esc comes back to the composer. In the composer Tab switches the destination, so Esc
//! with nothing to close, then Tab, leaves it.

use gpui::{App, Context, Focusable, KeyBinding, Window, actions};

use crate::chat_view::ChatView;

actions!(focus, [FocusNext, FocusPrev, SwitchRegion, ToComposer]);

pub const CONTEXT: &str = "ChatView";

/// Binds the window-wide focus keys. The composer's own bindings take precedence inside it.
pub fn init(cx: &mut App) {
    let view = Some(CONTEXT);
    cx.bind_keys([
        KeyBinding::new("tab", FocusNext, view),
        KeyBinding::new("shift-tab", FocusPrev, view),
        KeyBinding::new("f6", SwitchRegion, view),
        KeyBinding::new("shift-f6", SwitchRegion, view),
        KeyBinding::new("escape", ToComposer, view),
    ]);
}

impl ChatView {
    pub(crate) fn composer_focused(&self, window: &Window, cx: &App) -> bool {
        self.input
            .read(cx)
            .focus_handle(cx)
            .contains_focused(window, cx)
    }

    pub(crate) fn focus_composer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.tab_leaves = false;
        self.input.update(cx, |s, cx| s.focus(window, cx));
        cx.notify();
    }

    /// Moves focus out of the composer to the next (or previous) control.
    pub(crate) fn leave_composer(
        &mut self,
        forward: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.tab_leaves = false;
        if forward {
            window.focus_next(cx);
        } else {
            window.focus_prev(cx);
        }
        cx.notify();
    }

    /// A mouse click on a control hands focus back to the composer, so typing still goes there.
    pub(crate) fn after_click(
        &mut self,
        click: &gpui::ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !click.is_keyboard() {
            self.focus_composer(window, cx);
        }
    }

    pub(crate) fn on_focus_next(
        &mut self,
        _: &FocusNext,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus_next(cx);
    }

    pub(crate) fn on_focus_prev(
        &mut self,
        _: &FocusPrev,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus_prev(cx);
    }

    pub(crate) fn on_switch_region(
        &mut self,
        _: &SwitchRegion,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The composer and the messages are the two regions; Tab reaches the buttons from either.
        if self.composer_focused(window, cx) {
            self.tab_leaves = false;
            self.focus_feed(window, cx);
        } else {
            self.focus_composer(window, cx);
        }
    }

    /// Esc: closes the search bar when in it; else back to the composer, or, already there with
    /// nothing to close, lets Tab leave it.
    pub(crate) fn on_to_composer(
        &mut self,
        _: &ToComposer,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.search_focused(window, cx) {
            self.close_search(window, cx);
        } else if self.composer_focused(window, cx) {
            self.tab_leaves = !self.tab_leaves;
            cx.notify();
        } else {
            // From the messages or a card's buttons, Esc also closes the card.
            self.panel = crate::panels::Panel::None;
            self.focus_composer(window, cx);
        }
    }
}
