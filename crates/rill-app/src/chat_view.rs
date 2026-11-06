//! The chat window: header with filters, the unified feed, panels and the composer.

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    Animation, AnimationExt, AnyElement, App, AppContext, Bounds, Context, Entity, FocusHandle,
    Focusable, InteractiveElement, IntoElement, ListAlignment, ListState, ParentElement, Pixels,
    Render, SharedString, StatefulInteractiveElement, Styled, Subscription, Task, Window, canvas,
    div, list, prelude::FluentBuilder, pulsating_between, px, rgb,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{InputEvent, TextareaState};
use gpui_component::scroll::{Scrollbar, ScrollbarMode};
use gpui_component::{Selectable, Sizable};
use rill_core::{
    ConnectionStatus, Destination, Event, Filter, Health, Hub, Outgoing, Platform, Session, now_ms,
};
use tokio::sync::mpsc;

use crate::focus;
use crate::image_cache::BoundedImageCache;
use crate::navigate;
use crate::panels::Panel;
use crate::platforms::Platforms;
use crate::style;

/// Feed entries kept in memory.
/// The history choices in Settings: chat lines, a short note, and Rill's memory use with that
/// history full (measured under simulated chat, so long messages use somewhat more).
pub const HISTORY_CHOICES: [(usize, &str, u32); 5] = [
    (1_000, "Lightest", 100),
    (5_000, "Recommended", 110),
    (20_000, "Long streams", 140),
    (50_000, "Big events", 190),
    (100_000, "Most", 280),
];
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
    pub(crate) platforms: Platforms,
    pub(crate) list: ListState,
    pub(crate) input: Entity<TextareaState>,
    pub(crate) destination: Destination,
    pub(crate) completion_ix: usize,
    /// Deleted messages keep their text, faded, instead of "message deleted".
    pub(crate) show_deleted: bool,
    /// Hovering a link fetches and shows its preview.
    pub(crate) link_previews: bool,
    /// Which lines stand out, and in what colors.
    pub(crate) highlights: rill_os::settings::Highlights,
    pub(crate) previews: crate::preview::Previews,
    /// The link under the pointer and where it starts (window coordinates).
    pub(crate) link_hover: Option<(String, gpui::Point<Pixels>)>,
    preview_task: Option<Task<()>>,
    /// The first row shown in a long suggestion list.
    pub(crate) completion_top: usize,
    /// Composer text for which the user dismissed autocomplete.
    pub(crate) completions_dismissed: Option<SharedString>,
    /// Tab stepping through emotes and names for the word before the cursor.
    pub(crate) tab_cycle: Option<crate::composer::TabCycle>,
    pub(crate) composer_error: Option<SharedString>,
    /// Set by Esc in the composer: the next Tab leaves it instead of switching the destination.
    pub(crate) tab_leaves: bool,
    pub(crate) panel: Panel,
    /// The panel's scroll position, for long panels like Settings.
    pub(crate) panel_scroll: gpui::ScrollHandle,
    /// Where the open user card's name was clicked (window y), to float the card beside it.
    pub(crate) card_at: Option<Pixels>,
    /// The feed's bounds as last drawn, for placing the user card.
    pub(crate) feed_bounds: Rc<Cell<Bounds<Pixels>>>,
    /// The "which platform?" prompt, which takes focus so the keyboard can answer it.
    pub(crate) target_focus: FocusHandle,
    /// The platform picked in that prompt with the keyboard.
    pub(crate) target_ix: usize,
    /// The Ctrl+F bar, while open.
    pub(crate) search: Option<crate::search::Search>,
    /// The feed, as a focus stop for keyboard access to messages.
    pub(crate) feed_focus: FocusHandle,
    /// The message picked with the arrow keys, by feed sequence number.
    pub(crate) selected: Option<u64>,
    /// The message the composer replies to, if any.
    pub(crate) replying: Option<crate::composer::Replying>,
    /// The destination before a reply, restored after it.
    pub(crate) before_reply: Option<Destination>,
    /// The Kick channel name box in Accounts.
    pub(crate) kick_channel: Entity<gpui_component::input::InputState>,
    /// The Rumble Live Stream API link box in Accounts; masked, since the link holds a key.
    pub(crate) rumble_link: Entity<gpui_component::input::InputState>,
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
        platforms: Platforms,
        events: mpsc::Receiver<Event>,
        startup_errors: Vec<String>,
        history: Option<usize>,
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
        let saved = platforms
            .saves()
            .then(|| rill_os::settings::load().ok())
            .flatten();
        let show_deleted = saved.as_ref().is_some_and(|s| s.show_deleted_messages);
        let link_previews = saved.as_ref().is_none_or(|s| s.link_previews);
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

        let history = history
            .or(saved.as_ref().map(|s| s.message_history))
            .unwrap_or(rill_core::feed::DEFAULT_CAPACITY)
            .clamp(100, 1_000_000);
        let mut session = Session::new(history);
        for e in startup_errors {
            session.notice(None, rill_core::Level::Error, &e, now_ms());
        }
        let mut view = Self {
            session,
            hub,
            platforms,
            list,
            input,
            destination: Destination::All,
            completion_ix: 0,
            show_deleted,
            link_previews,
            highlights: saved
                .as_ref()
                .map(|s| s.highlights.clone())
                .unwrap_or_default(),
            previews: Default::default(),
            link_hover: None,
            preview_task: None,
            completion_top: 0,
            completions_dismissed: None,
            tab_cycle: None,
            composer_error: None,
            tab_leaves: false,
            panel: Panel::None,
            panel_scroll: gpui::ScrollHandle::new(),
            card_at: None,
            feed_bounds: Rc::default(),
            target_focus: cx.focus_handle(),
            target_ix: 0,
            search: None,
            feed_focus: cx.focus_handle().tab_stop(true),
            selected: None,
            replying: None,
            before_reply: None,
            kick_channel: cx.new(|cx| {
                gpui_component::input::InputState::new(window, cx).placeholder("Kick channel name")
            }),
            rumble_link: cx.new(|cx| {
                gpui_component::input::InputState::new(window, cx)
                    .placeholder("Live Stream API link")
                    .masked(true)
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
            // Late events from a platform that was just turned off.
            if !self.platforms.is_on(event.platform()) {
                continue;
            }
            if let Event::SignIn { prompt, .. } = &event {
                self.panel = Panel::Accounts;
                // Straight to the sign-in page; Accounts keeps Open page and Copy link.
                cx.open_url(&prompt.url);
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
                ConnectionStatus::Connected
                    | ConnectionStatus::Reconnecting { .. }
                    | ConnectionStatus::Waiting(_)
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

    /// Keeps this many chat lines from now on, and saves the choice. A smaller number drops the
    /// oldest lines at once.
    pub(crate) fn set_history(&mut self, lines: usize, cx: &mut Context<Self>) {
        self.session.feed.set_capacity(lines);
        if self.platforms.saves()
            && let Err(e) = rill_os::settings::update(|s| s.message_history = lines)
        {
            self.session
                .notice(None, rill_core::Level::Error, &e, now_ms());
        }
        self.sync_list();
        cx.notify();
    }

    /// Chat lines arriving per minute, over the last five minutes (or since the first line).
    pub(crate) fn lines_per_minute(&self) -> Option<f64> {
        let now = now_ms();
        let since = now - 5 * 60_000;
        let mut count = 0usize;
        let mut first = now;
        for e in self.session.feed.entries().rev() {
            if e.received_ms < since {
                break;
            }
            count += 1;
            first = e.received_ms;
        }
        let minutes = ((now - first) as f64 / 60_000.).max(0.5);
        (count >= 10).then(|| count as f64 / minutes)
    }

    /// Changes the highlight settings and saves them.
    pub(crate) fn set_highlights(
        &mut self,
        change: impl Fn(&mut rill_os::settings::Highlights),
        cx: &mut Context<Self>,
    ) {
        change(&mut self.highlights);
        if self.platforms.saves()
            && let Err(e) = rill_os::settings::update(|s| change(&mut s.highlights))
        {
            self.session
                .notice(None, rill_core::Level::Error, &e, now_ms());
            self.sync_list();
        }
        self.list.remeasure();
        cx.notify();
    }

    /// The color a line stands out in, if any: mentions first, then events.
    pub(crate) fn highlight_color(&self, entry: &rill_core::Entry) -> Option<u32> {
        let h = &self.highlights;
        if entry.highlighted && h.mentions {
            Some(style::parse_hex(&h.mention_color).unwrap_or(style::MENTION_BAR))
        } else if entry.is_event() && h.events {
            Some(style::parse_hex(&h.event_color).unwrap_or(0xa970ff))
        } else {
            None
        }
    }

    /// Marks beside the scroll bar where highlighted lines are; clicking one scrolls to it.
    fn render_marks(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let h = &self.highlights;
        if !h.scrollbar_marks || !(h.mentions || h.events) {
            return None;
        }
        let total = self.session.feed.visible_len();
        if total == 0 {
            return None;
        }
        // One mark per slot, so a busy stretch doesn't draw thousands; mentions win a slot.
        const SLOTS: usize = 300;
        let mut slots: Vec<Option<(usize, bool)>> = vec![None; SLOTS];
        for m in self.session.feed.marks() {
            if !(if m.mention { h.mentions } else { h.events }) {
                continue;
            }
            let slot = (m.ix * SLOTS / total).min(SLOTS - 1);
            match slots[slot] {
                Some((_, true)) => {}
                _ => slots[slot] = Some((m.ix, m.mention)),
            }
        }
        let mention = style::parse_hex(&h.mention_color).unwrap_or(style::MENTION_BAR);
        let event = style::parse_hex(&h.event_color).unwrap_or(0xa970ff);
        let marks = slots.into_iter().flatten().map(|(ix, is_mention)| {
            let top = ix as f32 / total as f32;
            // A 3 px mark with a taller target around it.
            div()
                .id(("mark", ix))
                .absolute()
                .top(gpui::relative(top))
                .mt(px(-2.))
                .left_0()
                .w_full()
                .h(px(7.))
                .py(px(2.))
                .cursor_pointer()
                // The scroll bar underneath mustn't also take the press.
                .occlude()
                .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |view, _, _, cx| view.scroll_to_line(ix, cx)))
                .child(div().size_full().rounded_sm().bg(rgb(if is_mention {
                    mention
                } else {
                    event
                })))
        });
        Some(
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .right(px(2.))
                .w(px(6.))
                .children(marks)
                .into_any_element(),
        )
    }

    /// Scrolls so a line is in view, a little below the top, and stops following.
    fn scroll_to_line(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.list.scroll_to(gpui::ListOffset {
            item_ix: ix.saturating_sub(2),
            offset_in_item: px(0.),
        });
        self.following = self.list.is_following_tail();
        cx.notify();
    }

    /// Turns link previews on or off, and saves the choice.
    pub(crate) fn set_link_previews(&mut self, on: bool, cx: &mut Context<Self>) {
        self.link_previews = on;
        if self.platforms.saves()
            && let Err(e) = rill_os::settings::update(|s| s.link_previews = on)
        {
            self.session
                .notice(None, rill_core::Level::Error, &e, now_ms());
            self.sync_list();
        }
        cx.notify();
    }

    /// The pointer moved onto a link or off one. A preview is fetched after a short pause on it.
    pub(crate) fn hover_link(
        &mut self,
        hover: Option<(String, gpui::Point<Pixels>)>,
        cx: &mut Context<Self>,
    ) {
        if self.link_hover == hover {
            return;
        }
        self.link_hover = hover.clone();
        cx.notify();
        let Some((url, _)) = hover else {
            self.preview_task = None;
            return;
        };
        let known = matches!(
            self.previews.get(&url),
            Some(crate::preview::State::Ready(_) | crate::preview::State::Failed)
        );
        if !self.link_previews || known || !crate::preview::may_fetch(&url) {
            return;
        }
        self.preview_task = Some(cx.spawn(async move |view, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(350))
                .await;
            let client = view.update(cx, |view, cx| {
                let still = view.link_hover.as_ref().is_some_and(|(u, _)| *u == url);
                still.then(|| {
                    view.previews
                        .set(url.clone(), crate::preview::State::Loading);
                    cx.notify();
                    cx.http_client()
                })
            });
            let Ok(Some(client)) = client else {
                return;
            };
            let fetch = crate::preview::fetch(client, url.clone());
            let timer = cx.background_executor().timer(crate::preview::TIMEOUT);
            futures::pin_mut!(fetch, timer);
            let state = match futures::future::select(fetch, timer).await {
                futures::future::Either::Left((state, _)) => state,
                futures::future::Either::Right(_) => crate::preview::State::Failed,
            };
            let _ = view.update(cx, |view, cx| {
                view.previews.set(url, state);
                cx.notify();
            });
        }));
    }

    /// Shows deleted messages' text (faded) or "message deleted", and saves the choice.
    pub(crate) fn set_show_deleted(&mut self, on: bool, cx: &mut Context<Self>) {
        self.show_deleted = on;
        if self.platforms.saves()
            && let Err(e) = rill_os::settings::update(|s| s.show_deleted_messages = on)
        {
            self.session
                .notice(None, rill_core::Level::Error, &e, now_ms());
            self.sync_list();
        }
        // Rows are cached by the list, so redraw them all.
        self.list.remeasure();
        cx.notify();
    }

    /// Turns a platform on (connecting it) or off (disconnecting it and hiding it everywhere). Sign-ins
    /// are kept either way. The last platform that's on stays on.
    pub(crate) fn set_platform_on(&mut self, p: Platform, on: bool, cx: &mut Context<Self>) {
        if on == self.platforms.is_on(p) || (!on && self.platforms.on() == [p]) {
            return;
        }
        if let Err(e) = self.platforms.set(p, on) {
            self.session
                .notice(None, rill_core::Level::Error, &e, now_ms());
        }
        let mut filter = self.session.feed.filter().clone();
        if on {
            filter.hidden.remove(&p);
            if let Err(e) = self.hub.add(self.platforms.make(p)) {
                let why = format!("{p} couldn't start: {e}. Restart Rill.");
                self.session
                    .notice(None, rill_core::Level::Error, &why, now_ms());
            }
        } else {
            self.hub.stop(p);
            self.session.forget(p);
            filter.hidden.insert(p);
            if self.destination == Destination::One(p) {
                self.destination = Destination::All;
            }
            if self.replying.as_ref().is_some_and(|r| r.platform == p) {
                self.replying = None;
                self.before_reply = None;
            }
            if matches!(&self.panel, Panel::User(card) if card.platform == p) {
                self.panel = Panel::Settings;
            }
        }
        self.set_filter(filter, cx);
    }

    pub(crate) fn set_filter(&mut self, filter: Filter, cx: &mut Context<Self>) {
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
                self.completion_top = 0;
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
        let toggles = self.platforms.on().into_iter().map(|p| {
            let status = self
                .session
                .link(p)
                .map(|l| l.status.clone())
                .unwrap_or_default();
            let shown = !filter.hidden.contains(&p);
            let (health, why) = self.session.health(p);
            // Yellow, red and grey need something done, which happens in Accounts.
            let fix = matches!(
                health,
                Health::Attention | Health::Broken | Health::SignedOut
            );
            let headline = match health {
                Health::Good => "connected".to_string(),
                Health::Waiting => "ready, waiting".into(),
                Health::Busy => status.label().to_lowercase(),
                Health::Attention => "connected, but needs attention".into(),
                Health::Broken => "not working".into(),
                Health::SignedOut => "not signed in".into(),
            };
            let action = if fix {
                "Click to open Accounts."
            } else if shown {
                "Click to hide its messages."
            } else {
                "Click to show its messages."
            };
            let mut tip = format!("{p}: {headline}.");
            for line in &why {
                tip.push('\n');
                tip.push_str(line);
            }
            tip.push('\n');
            tip.push_str(action);
            let tip: SharedString = tip.into();
            Button::new(SharedString::from(format!("filter-{p}")))
                .ghost()
                .xsmall()
                .relative()
                .toggled(shown)
                .accessibility_label(tip.clone())
                .tooltip(tip)
                .when(!shown, |d| d.opacity(0.35))
                .child(style::platform_icon(p, 16.))
                .child(status_dot(p, health))
                .on_click(cx.listener(move |view, click, window, cx| {
                    if fix {
                        view.panel = Panel::Accounts;
                        cx.notify();
                    } else {
                        view.toggle_platform(p, cx);
                    }
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
                "settings",
                "Settings",
                matches!(self.panel, Panel::Settings),
                "Choose which platforms Rill uses",
                cx.listener(|view, click, window, cx| {
                    view.panel = match view.panel {
                        Panel::Settings => Panel::None,
                        _ => Panel::Settings,
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
                        // It floats over the messages: clicks stop here, scrolling still passes.
                        .block_mouse_except_scroll()
                        .child(label)
                        .on_click(cx.listener(|view, _, _, cx| {
                            cx.stop_propagation();
                            view.jump_to_latest(cx);
                        })),
                )
        });
        div()
            .relative()
            .flex_1()
            .min_h_0()
            .track_focus(&self.feed_focus)
            .key_context(navigate::CONTEXT)
            .on_action(cx.listener(Self::on_select_prev))
            .on_action(cx.listener(Self::on_select_next))
            .on_action(cx.listener(Self::on_select_first))
            .on_action(cx.listener(Self::on_select_last))
            .on_action(cx.listener(Self::on_open_selected))
            .on_action(cx.listener(Self::on_reply_selected))
            .image_cache(self.images.clone())
            .child(
                list(
                    self.list.clone(),
                    cx.processor(|view, ix, window, cx| view.render_row(ix, window, cx)),
                )
                .size_full(),
            )
            // Out of the way while following live chat; once scrolled up, older messages are a drag away.
            .when(!self.following, |d| {
                d.child(Scrollbar::vertical(&self.list).mode(ScrollbarMode::Always))
            })
            // Over the scroll bar, so a click on a mark goes to its line.
            .children(self.render_marks(cx))
            .children(jump)
            .child({
                let bounds = self.feed_bounds.clone();
                // Pinned to the corner: an absolute child without insets sits after the list.
                canvas(move |b, _, _| bounds.set(b), |_, _, _, _| {})
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
            })
            .children(self.render_card(cx))
            .children(self.render_link_preview())
    }
}

/// The dot on a platform's header icon: green working, a green ring waiting, pulsing yellow
/// connecting, yellow needing attention, red broken, grey not signed in.
fn status_dot(p: Platform, health: Health) -> AnyElement {
    let (color, ring) = match health {
        Health::Good => (style::OK, false),
        Health::Waiting => (style::OK, true),
        Health::Busy | Health::Attention => (style::WARN, false),
        Health::Broken => (style::ERROR, false),
        Health::SignedOut => (style::FAINT, false),
    };
    let dot = div()
        .absolute()
        .right(px(0.))
        .bottom(px(0.))
        .size(px(7.))
        .rounded_full();
    let dot = if ring {
        dot.border_2()
            .border_color(rgb(color))
            .bg(rgb(style::BACKGROUND))
    } else {
        dot.border_1()
            .border_color(rgb(style::BACKGROUND))
            .bg(rgb(color))
    };
    if health == Health::Busy {
        dot.with_animation(
            SharedString::from(format!("dot-{p}")),
            Animation::new(Duration::from_millis(1200))
                .repeat()
                .with_easing(pulsating_between(0.3, 1.)),
            |d, t| d.opacity(t),
        )
        .into_any_element()
    } else {
        dot.into_any_element()
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
            .on_action(cx.listener(Self::on_find))
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(style::BACKGROUND))
            .text_color(rgb(style::TEXT))
            .text_sm()
            .child(self.render_header(cx))
            .children(self.render_search(cx))
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
