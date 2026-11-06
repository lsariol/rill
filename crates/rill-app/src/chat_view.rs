//! The chat window: header with filters, the unified feed, panels and the composer.

use std::time::{Duration, Instant};

use gpui::{
    App, AppContext, Context, Entity, FocusHandle, Focusable, InteractiveElement, IntoElement,
    ListAlignment, ListState, ParentElement, Render, SharedString, StatefulInteractiveElement,
    Styled, Subscription, Task, Window, div, list, prelude::FluentBuilder, px, rgb,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{InputEvent, TextareaState};
use gpui_component::scroll::{Scrollbar, ScrollbarMode};
use gpui_component::{Selectable, Sizable};
use rill_core::{
    ConnectionStatus, Destination, Event, Filter, Hub, Outgoing, Platform, Session, now_ms,
};
use tokio::sync::mpsc;

use crate::focus;
use crate::image_cache::BoundedImageCache;
use crate::panels::Panel;
use crate::style;

/// Feed entries kept in memory.
pub const FEED_CAPACITY: usize = 5_000;
/// Emote and badge images kept loaded.
const IMAGE_LIMIT: usize = 512;
/// Below this width the feed switches to compact mode by itself.
const NARROW: f32 = 340.;

/// Counters for benchmarks.
#[derive(Debug, Default, Clone, Copy)]
pub struct Stats {
    pub events: u64,
    pub batches: u64,
    pub renders: u64,
    pub rows: u64,
}

/// The root view of a chat window.
pub struct ChatView {
    pub(crate) session: Session,
    pub(crate) hub: Hub,
    pub(crate) list: ListState,
    pub(crate) input: Entity<TextareaState>,
    pub(crate) destination: Destination,
    pub(crate) completion_ix: usize,
    /// Composer text for which the user dismissed autocomplete.
    pub(crate) completions_dismissed: Option<SharedString>,
    pub(crate) composer_error: Option<SharedString>,
    /// Set by Esc in the composer: the next Tab leaves it instead of switching the destination.
    pub(crate) tab_leaves: bool,
    pub(crate) panel: Panel,
    /// The Kick channel name box in Accounts.
    pub(crate) kick_channel: Entity<gpui_component::input::InputState>,
    pub(crate) compact: bool,
    effective_compact: bool,
    following: bool,
    unseen: usize,
    pub(crate) images: Entity<BoundedImageCache>,
    focus: FocusHandle,
    pub stats: Stats,
    started: Instant,
    _tasks: Vec<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl ChatView {
    /// Creates the view and starts draining the hub's events.
    pub fn new(
        hub: Hub,
        events: mpsc::Receiver<Event>,
        startup_errors: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, 6)
                .submit_on_enter(true)
                .placeholder("Send a message")
        });
        let subscription = cx.subscribe_in(&input, window, Self::on_input);
        input.update(cx, |state, cx| state.focus(window, cx));

        let list = ListState::new(0, ListAlignment::Bottom, px(400.));
        list.set_follow_mode(gpui::FollowMode::Tail);
        let weak = cx.weak_entity();
        list.set_scroll_handler(move |event, _, cx| {
            let following = event.is_following_tail;
            weak.update(cx, |view, cx| {
                if view.following != following {
                    view.following = following;
                    if following {
                        view.unseen = 0;
                    }
                    cx.notify();
                }
            })
            .ok();
        });

        let pump = cx.spawn(async move |this, cx| pump_events(this, events, cx).await);

        let mut session = Session::new(FEED_CAPACITY);
        for e in startup_errors {
            session.notice(None, rill_core::Level::Error, &e, now_ms());
        }
        let mut view = Self {
            session,
            hub,
            list,
            input,
            destination: Destination::All,
            completion_ix: 0,
            completions_dismissed: None,
            composer_error: None,
            tab_leaves: false,
            panel: Panel::None,
            kick_channel: cx.new(|cx| {
                gpui_component::input::InputState::new(window, cx).placeholder("Kick channel name")
            }),
            compact: false,
            effective_compact: false,
            following: true,
            unseen: 0,
            images: BoundedImageCache::new(IMAGE_LIMIT, cx),
            focus: cx.focus_handle(),
            stats: Stats::default(),
            started: Instant::now(),
            _tasks: vec![pump],
            _subscriptions: vec![subscription],
        };
        view.sync_list();
        view
    }

    /// Applies a batch of adapter events and updates the list.
    fn apply_events(&mut self, batch: Vec<Event>, cx: &mut Context<Self>) {
        let now = now_ms();
        self.stats.events += batch.len() as u64;
        self.stats.batches += 1;
        for event in batch {
            if matches!(event, Event::SignIn { .. }) {
                self.panel = Panel::Accounts;
            }
            self.session.apply(event, now);
        }
        if let Destination::One(p) = self.destination
            && !self.session.sendable().contains(&p)
        {
            self.destination = Destination::All;
        }
        self.sync_list();
        cx.notify();
    }

    /// Picks up following changes the scroll handler doesn't see, such as scrollbar drags.
    fn refresh_following(&mut self) {
        let following = self.list.is_following_tail();
        if following != self.following {
            self.following = following;
            if following {
                self.unseen = 0;
            }
        }
    }

    /// Brings the list in line with the feed's changes.
    pub(crate) fn sync_list(&mut self) {
        self.refresh_following();
        let changes = self.session.feed.take_changes();
        if changes.reset {
            self.list.reset(self.session.feed.visible_len());
            return;
        }
        if changes.evicted > 0 {
            self.list.splice(0..changes.evicted, 0);
        }
        if changes.appended > 0 {
            let n = self.list.item_count();
            self.list.splice(n..n, changes.appended);
            if !self.following {
                self.unseen += changes.appended;
            }
        }
        for seq in changes.updated {
            if let Some(ix) = self.session.feed.visible_index(seq) {
                self.list.remeasure_items(ix..ix + 1);
            }
        }
    }

    /// Hands requests to the adapters; a refusal becomes a failed result right away.
    pub(crate) fn dispatch(&mut self, out: Vec<Outgoing>) {
        for o in out {
            let id = o.request.id;
            if let Err(e) = self.hub.send(o.platform, o.request) {
                self.session.apply(
                    Event::Done {
                        platform: o.platform,
                        request: id,
                        result: Err(e),
                    },
                    now_ms(),
                );
            }
        }
        self.sync_list();
    }

    /// Whether the feed is following new messages.
    pub(crate) fn is_following(&self) -> bool {
        self.following
    }

    /// Scrolls the feed by whole viewports (negative is up).
    pub(crate) fn scroll_feed_pages(&mut self, pages: f32, cx: &mut Context<Self>) {
        let height = self.list.viewport_bounds().size.height;
        let mut distance = height * (pages * 0.9);
        // While following, the list's offset is the end of the content, a full viewport below its top.
        if self.following && pages < 0. {
            distance -= height;
        }
        self.list.scroll_by(distance);
        self.following = self.list.is_following_tail();
        cx.notify();
    }

    /// Jumps to the newest message and follows again.
    pub(crate) fn jump_to_latest(&mut self, cx: &mut Context<Self>) {
        self.list.set_follow_mode(gpui::FollowMode::Tail);
        self.following = true;
        self.unseen = 0;
        cx.notify();
    }

    /// A header icon click: sign in if the platform isn't connected, else show or hide it.
    fn toggle_platform(&mut self, p: Platform, cx: &mut Context<Self>) {
        let usable = self.session.link(p).is_some_and(|l| {
            matches!(
                l.status,
                ConnectionStatus::Connected | ConnectionStatus::Reconnecting { .. }
            )
        });
        if !usable {
            self.panel = Panel::Accounts;
            cx.notify();
            return;
        }
        let mut filter = self.session.feed.filter().clone();
        if !filter.hidden.remove(&p) {
            filter.hidden.insert(p);
        }
        self.set_filter(filter, cx);
    }

    fn set_filter(&mut self, filter: Filter, cx: &mut Context<Self>) {
        self.session.feed.set_filter(filter);
        self.sync_list();
        self.jump_to_latest(cx);
    }

    fn on_input(
        &mut self,
        _: &Entity<TextareaState>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Change => {
                self.completion_ix = 0;
                self.composer_error = None;
                self.tab_leaves = false;
                cx.notify();
            }
            InputEvent::PressEnter { shift: false, .. } => self.on_enter(window, cx),
            _ => {}
        }
    }

    /// Seconds since the window opened.
    pub fn uptime(&self) -> Duration {
        self.started.elapsed()
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let filter = self.session.feed.filter().clone();
        let toggles = Platform::ALL.into_iter().map(|p| {
            let status = self
                .session
                .link(p)
                .map(|l| l.status.clone())
                .unwrap_or_default();
            let shown = !filter.hidden.contains(&p);
            let dot = match status {
                ConnectionStatus::Connected => Some(style::OK),
                ConnectionStatus::Connecting | ConnectionStatus::Reconnecting { .. } => {
                    Some(style::WARN)
                }
                ConnectionStatus::Failed(_) => Some(style::ERROR),
                ConnectionStatus::Disconnected => None,
            };
            let tip: SharedString = match status {
                ConnectionStatus::Connected | ConnectionStatus::Reconnecting { .. } => format!(
                    "{p}: {}. Click to {} its messages.",
                    status.label(),
                    if shown { "hide" } else { "show" }
                ),
                ConnectionStatus::Failed(why) => format!("{why} Click to open Accounts."),
                _ => format!("{p}: {}. Click to connect.", status.label()),
            }
            .into();
            Button::new(SharedString::from(format!("filter-{p}")))
                .ghost()
                .xsmall()
                .relative()
                .toggled(shown)
                .accessibility_label(tip.clone())
                .tooltip(tip)
                .when(!shown, |d| d.opacity(0.35))
                .child(style::platform_icon(p, 16.))
                .when_some(dot, |d, color| {
                    d.child(
                        div()
                            .absolute()
                            .right(px(1.))
                            .bottom(px(1.))
                            .size(px(6.))
                            .rounded_full()
                            .bg(rgb(color)),
                    )
                })
                .on_click(cx.listener(move |view, click, window, cx| {
                    view.toggle_platform(p, cx);
                    view.after_click(click, window, cx);
                }))
        });
        let mentions = filter.mentions_only;
        let compact = self.compact;
        div()
            .flex()
            .items_center()
            .gap_1()
            .px_2()
            .py_1()
            .border_b_1()
            .border_color(rgb(style::BORDER))
            .children(toggles)
            .child(div().flex_1())
            .child(header_toggle(
                "accounts",
                "Accounts",
                matches!(self.panel, Panel::Accounts),
                "Connect or disconnect your platform accounts",
                cx.listener(|view, click, window, cx| {
                    view.panel = match view.panel {
                        Panel::Accounts => Panel::None,
                        _ => Panel::Accounts,
                    };
                    view.after_click(click, window, cx);
                }),
            ))
            .child(header_toggle(
                "mentions",
                "@",
                mentions,
                "Show only messages that mention you",
                cx.listener(move |view, click, window, cx| {
                    let mut f = view.session.feed.filter().clone();
                    f.mentions_only = !f.mentions_only;
                    view.set_filter(f, cx);
                    view.after_click(click, window, cx);
                }),
            ))
            .child(header_toggle(
                "compact",
                "≡",
                compact,
                "Compact mode: hide times and badges",
                cx.listener(|view, click, window, cx| {
                    view.compact = !view.compact;
                    view.after_click(click, window, cx);
                }),
            ))
    }

    fn render_feed(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        self.refresh_following();
        let jump = (!self.following).then(|| {
            let label: SharedString = if self.unseen > 0 {
                format!("↓ {} new messages", self.unseen).into()
            } else {
                "↓ Paused. Jump to latest".into()
            };
            div()
                .absolute()
                .bottom_2()
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(
                    div()
                        .id("jump")
                        .px_3()
                        .py_1()
                        .rounded_full()
                        .bg(rgb(style::RAISED))
                        .border_1()
                        .border_color(rgb(style::BORDER))
                        .text_xs()
                        .text_color(rgb(style::TEXT))
                        .cursor_pointer()
                        .hover(|s| s.bg(rgb(style::BORDER)))
                        .child(label)
                        .on_click(cx.listener(|view, _, _, cx| view.jump_to_latest(cx))),
                )
        });
        div()
            .relative()
            .flex_1()
            .min_h_0()
            .image_cache(self.images.clone())
            .child(
                list(
                    self.list.clone(),
                    cx.processor(|view, ix, window, cx| view.render_row(ix, window, cx)),
                )
                .size_full(),
            )
            // Always shown, so older messages are one drag away.
            .child(Scrollbar::vertical(&self.list).mode(ScrollbarMode::Always))
            .children(jump)
    }
}

/// A small on/off button in the header.
fn header_toggle(
    id: &'static str,
    label: &'static str,
    on: bool,
    tip: &'static str,
    handler: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    Button::new(id)
        .ghost()
        .xsmall()
        .label(label)
        .selected(on)
        .toggled(on)
        .text_color(rgb(if on { style::TEXT } else { style::FAINT }))
        .accessibility_label(tip)
        .tooltip(tip)
        .on_click(handler)
}

/// Drains adapter events into the view, at most once per frame and never while idle.
async fn pump_events(
    this: gpui::WeakEntity<ChatView>,
    mut events: mpsc::Receiver<Event>,
    cx: &mut gpui::AsyncApp,
) {
    const MAX_BATCH: usize = 4_096;
    while let Some(first) = events.recv().await {
        let mut batch = vec![first];
        while batch.len() < MAX_BATCH {
            match events.try_recv() {
                Ok(e) => batch.push(e),
                Err(_) => break,
            }
        }
        if this
            .update(cx, |view, cx| view.apply_events(batch, cx))
            .is_err()
        {
            return;
        }
        cx.background_executor()
            .timer(Duration::from_millis(16))
            .await;
    }
}

impl Focusable for ChatView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for ChatView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.stats.renders += 1;
        let compact = self.compact || window.viewport_size().width < px(NARROW);
        if compact != self.effective_compact {
            self.effective_compact = compact;
            self.list.remeasure();
        }
        div()
            .key_context(focus::CONTEXT)
            .on_action(cx.listener(Self::on_focus_next))
            .on_action(cx.listener(Self::on_focus_prev))
            .on_action(cx.listener(Self::on_switch_region))
            .on_action(cx.listener(Self::on_to_composer))
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(style::BACKGROUND))
            .text_color(rgb(style::TEXT))
            .text_sm()
            .child(self.render_header(cx))
            .child(self.render_feed(cx))
            .child(self.render_panel(window, cx))
            .child(self.render_composer(window, cx))
    }
}

impl ChatView {
    /// Whether rows are drawn compact right now.
    pub(crate) fn is_compact(&self) -> bool {
        self.effective_compact
    }
}
