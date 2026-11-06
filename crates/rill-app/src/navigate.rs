//! Keyboard access to messages: F6 moves into the feed, the arrow keys pick a message, and Enter
//! opens its user card.

use gpui::{App, Context, KeyBinding, Window, actions};
use rill_core::Item;

use crate::chat_view::ChatView;
use crate::composer::Replying;
use crate::panels::{Panel, UserCard};

actions!(
    feed,
    [
        SelectPrev,
        SelectNext,
        SelectFirst,
        SelectLast,
        OpenSelected,
        ReplySelected
    ]
);

pub const CONTEXT: &str = "Feed";

pub fn init(cx: &mut App) {
    let feed = Some(CONTEXT);
    cx.bind_keys([
        KeyBinding::new("up", SelectPrev, feed),
        KeyBinding::new("down", SelectNext, feed),
        KeyBinding::new("home", SelectFirst, feed),
        KeyBinding::new("end", SelectLast, feed),
        KeyBinding::new("enter", OpenSelected, feed),
        KeyBinding::new("space", OpenSelected, feed),
        KeyBinding::new("r", ReplySelected, feed),
    ]);
}

impl ChatView {
    pub(crate) fn feed_focused(&self, window: &Window, cx: &App) -> bool {
        self.feed_focus.contains_focused(window, cx)
    }

    /// Moves focus into the feed, picking the newest message if none is picked.
    pub(crate) fn focus_feed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let len = self.session.feed.visible_len();
        let ix = self
            .selected
            .and_then(|seq| self.session.feed.visible_index(seq))
            .or(len.checked_sub(1));
        if let Some(ix) = ix {
            self.select(ix);
        }
        window.focus(&self.feed_focus, cx);
        cx.notify();
    }

    fn select(&mut self, ix: usize) {
        if let Some(entry) = self.session.feed.visible(ix) {
            self.selected = Some(entry.seq);
            self.list.scroll_to_reveal_item(ix);
        }
    }

    /// The picked message's position, or the newest when the pick has scrolled out of the feed.
    fn selected_ix(&self) -> Option<usize> {
        self.selected
            .and_then(|seq| self.session.feed.visible_index(seq))
            .or(self.session.feed.visible_len().checked_sub(1))
    }

    pub(crate) fn on_select_prev(
        &mut self,
        _: &SelectPrev,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(ix) = self.selected_ix() {
            self.select(ix.saturating_sub(1));
            cx.notify();
        }
    }

    pub(crate) fn on_select_next(
        &mut self,
        _: &SelectNext,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let last = self.session.feed.visible_len().saturating_sub(1);
        if let Some(ix) = self.selected_ix() {
            self.select((ix + 1).min(last));
            cx.notify();
        }
    }

    pub(crate) fn on_select_first(
        &mut self,
        _: &SelectFirst,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select(0);
        cx.notify();
    }

    pub(crate) fn on_select_last(
        &mut self,
        _: &SelectLast,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(last) = self.session.feed.visible_len().checked_sub(1) {
            self.select(last);
            cx.notify();
        }
    }

    /// R on a chat message starts a reply to it, where the platform takes replies.
    pub(crate) fn on_reply_selected(
        &mut self,
        _: &ReplySelected,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(entry) = self
            .selected_ix()
            .and_then(|ix| self.session.feed.visible(ix))
        else {
            return;
        };
        if let Item::Chat(m) = &entry.item
            && !entry.deleted
            && self.session.can(m.platform, rill_core::Action::Reply)
        {
            let replying = Replying {
                platform: m.platform,
                message_id: m.id.clone(),
                author: m.author.display_name.clone(),
                text: m.plain_text(),
            };
            self.start_reply(replying, window, cx);
        }
    }

    /// Enter on a chat message opens its user card (delete, timeouts, ban).
    pub(crate) fn on_open_selected(
        &mut self,
        _: &OpenSelected,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(entry) = self
            .selected_ix()
            .and_then(|ix| self.session.feed.visible(ix))
        else {
            return;
        };
        if let Item::Chat(m) = &entry.item {
            let card = UserCard {
                platform: m.platform,
                author: m.author.clone(),
                message_id: Some(m.id.clone()),
                deleted: entry.deleted,
                text: m.plain_text(),
            };
            self.open_panel(Panel::User(card), window, cx);
        }
    }
}
