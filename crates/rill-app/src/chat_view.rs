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
use gpui_component::scroll::Scrollbar;
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
/// How the view starts: from the command line, not Settings.
#[derive(Debug, Default, Clone, Copy)]
pub struct ViewOptions {
    /// Chat lines to keep, overriding Settings (`--history`).
    pub history: Option<usize>,
    /// Simulated chat: first-time chatters are marked after a minute instead of a week.
    pub simulated: bool,
}

/// Highlighted lines' middles, in pixels from the top of the history, and their colors.
type MarkLines = Rc<Vec<(f32, u32)>>;

/// The highlight rows in Settings that have a color: mentions, first-time chatters, then each
/// kind of event.
pub(crate) fn color_keys() -> Vec<&'static str> {
    let mut keys = vec!["mention", "first"];
    keys.extend(rill_core::EventKind::ALL.iter().map(|k| k.key()));
    keys
}

/// Where updating stands.
#[derive(Clone, Default)]
pub(crate) enum UpdateStatus {
    #[default]
    Idle,
    Checking,
    /// A development run: only an installed Rill updates.
    NotInstalled,
    UpToDate,
    Ready(crate::updates::Ready),
    Failed(String),
}

/// How often an installed Rill looks for a new version.
const UPDATE_EVERY: Duration = Duration::from_secs(6 * 60 * 60);

/// The text sizes in Settings, in percent.
pub const TEXT_SIZES: [u32; 5] = [90, 100, 115, 130, 150];

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
    /// Text size in percent of normal.
    pub(crate) text_size: u32,
    /// Look for new versions now and then.
    pub(crate) auto_update: bool,
    pub(crate) update: UpdateStatus,
    update_task: Option<Task<()>>,
    /// The hex box for each highlight color in Settings, by key (see `color_keys`).
    pub(crate) color_inputs: Vec<(&'static str, Entity<gpui_component::input::InputState>)>,
    /// A failure to keep known chatters was shown once already.
    known_error_shown: bool,
    pub(crate) previews: crate::preview::Previews,
    /// What the pointer is over (a link, emote or badge), and which line or badge reported it.
    pub(crate) hover: Option<(SharedString, crate::chat_text::Hover)>,
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
    /// Rows added while paused need estimated heights (see `refresh_row_hints`).
    rows_need_hints: bool,
    rows_hinted: Instant,
    /// The height given to rows not drawn yet.
    hint_height: f32,
    /// Heights of rows that have been drawn, by feed seq.
    row_heights: std::collections::HashMap<u64, f32>,
    /// Highlighted lines' offsets and the history's height, with when they were worked out.
    mark_cache: Option<(Instant, MarkLines, f32)>,
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
        options: ViewOptions,
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

        let ViewOptions { history, simulated } = options;
        let history = history
            .or(saved.as_ref().map(|s| s.message_history))
            .unwrap_or(rill_core::feed::DEFAULT_CAPACITY)
            .clamp(100, 1_000_000);
        let mut session = Session::new(history);
        // Who has chatted before. Simulated chat learns for a minute, then marks new names.
        if platforms.saves() {
            match rill_os::known::load() {
                Ok(k) => {
                    let p = |name: &str| Platform::from_name(name);
                    session.load_known(
                        k.since.into_iter().filter_map(|(n, t)| Some((p(&n)?, t))),
                        k.ids.into_iter().filter_map(|(n, id)| Some((p(&n)?, id))),
                    );
                }
                Err(e) => session.notice(None, rill_core::Level::Warn, &e, now_ms()),
            }
        } else if simulated {
            session.set_learning(60_000);
        }
        let highlights = saved
            .as_ref()
            .map(|s| s.highlights.clone())
            .unwrap_or_default();
        let text_size = saved
            .as_ref()
            .map_or(100, |s| s.text_size)
            .clamp(TEXT_SIZES[0], TEXT_SIZES[TEXT_SIZES.len() - 1]);
        crate::row::set_scale(text_size);
        let auto_update = saved.as_ref().is_none_or(|s| s.auto_update);
        let mut color_subscriptions = Vec::new();
        let color_inputs: Vec<_> = color_keys()
            .into_iter()
            .map(|key| {
                let input = cx.new(|cx| {
                    gpui_component::input::InputState::new(window, cx).placeholder("#rrggbb")
                });
                color_subscriptions.push(cx.subscribe_in(
                    &input,
                    window,
                    move |view: &mut Self, input, event: &InputEvent, window, cx| {
                        view.on_color_input(key, input, event, window, cx)
                    },
                ));
                (key, input)
            })
            .collect();
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
            highlights,
            text_size,
            auto_update,
            update: UpdateStatus::Idle,
            update_task: None,
            color_inputs,
            known_error_shown: false,
            previews: Default::default(),
            hover: None,
            preview_task: None,
            completion_top: 0,
            completions_dismissed: None,
            tab_cycle: None,
            composer_error: None,
            tab_leaves: false,
            panel: Panel::None,
            rows_need_hints: false,
            rows_hinted: Instant::now(),
            hint_height: 27.,
            row_heights: Default::default(),
            mark_cache: None,
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
            _tasks: vec![pump, Self::update_loop(cx)],
            _subscriptions: {
                let mut all = vec![subscription];
                all.extend(color_subscriptions);
                all
            },
        };
        // Each hex box starts with its color.
        for (key, input) in view.color_inputs.clone() {
            let hex = style::to_hex(view.color_of(key));
            input.update(cx, |s, cx| s.set_value(hex, window, cx));
        }
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
        self.keep_known_chatters();
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
            let estimate = self.row_height_estimate();
            self.hint_height = f32::from(estimate);
            self.list
                .reset_with_uniform_height(self.session.feed.visible_len(), estimate);
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
                // Lines added below while paused aren't drawn yet, so they have no height.
                self.rows_need_hints = true;
            }
        }
        for seq in changes.updated {
            if let Some(ix) = self.session.feed.visible_index(seq) {
                self.list.remeasure_items(ix..ix + 1);
            }
        }
    }

    /// The average height of the rows on screen, to stand in for rows not drawn yet.
    fn row_height_estimate(&self) -> Pixels {
        let top = self.list.logical_scroll_top().item_ix;
        let heights: Vec<f32> = (top..top + 40)
            .filter_map(|ix| self.list.bounds_for_item(ix))
            .map(|b| f32::from(b.size.height))
            .collect();
        if heights.is_empty() {
            px(if self.is_compact() { 21. } else { 27. })
        } else {
            px(heights.iter().sum::<f32>() / heights.len() as f32)
        }
    }

    /// Gives rows not drawn yet an estimated height, so the scroll bar spans the whole history
    /// (and its marks line up). At most once a second: it touches every row.
    fn refresh_row_hints(&mut self) {
        if self.rows_need_hints && self.rows_hinted.elapsed() >= Duration::from_secs(1) {
            let estimate = self.row_height_estimate();
            self.hint_height = f32::from(estimate);
            let _ = self.list.clone().with_uniform_item_height(estimate);
            self.rows_need_hints = false;
            self.rows_hinted = Instant::now();
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
        self.mark_cache = None;
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
        self.line_color(entry.highlighted, entry.first, entry.event())
    }

    /// A line's highlight: a mention first, then a first-time chatter, then its kind of event,
    /// each only while it's turned on.
    fn line_color(
        &self,
        mention: bool,
        first: bool,
        event: Option<rill_core::EventKind>,
    ) -> Option<u32> {
        let h = &self.highlights;
        if mention && h.mentions {
            Some(self.color_of("mention"))
        } else if first && h.first_chatters {
            Some(self.color_of("first"))
        } else {
            let kind = event?;
            (h.events && !h.events_off.iter().any(|k| k == kind.key()))
                .then(|| self.color_of(kind.key()))
        }
    }

    /// A highlight's color: the one chosen in Settings, else Rill's.
    pub(crate) fn color_of(&self, key: &str) -> u32 {
        let h = &self.highlights;
        let (chosen, default) = match key {
            "mention" => (Some(&h.mention_color), style::MENTION_BAR),
            "first" => (Some(&h.first_chatter_color), style::FIRST_CHATTER),
            k => {
                let kind = rill_core::EventKind::ALL
                    .into_iter()
                    .find(|e| e.key() == k)
                    .unwrap_or_default();
                (h.event_colors.get(k), style::event_default(kind))
            }
        };
        chosen.and_then(|c| style::parse_hex(c)).unwrap_or(default)
    }

    /// Whether a highlight row is on: `mention`, `first`, `events` (all events), `marks`, or a
    /// kind of event.
    pub(crate) fn highlight_on(&self, key: &str) -> bool {
        let h = &self.highlights;
        match key {
            "mention" => h.mentions,
            "first" => h.first_chatters,
            "events" => h.events,
            "marks" => h.scrollbar_marks,
            k => !h.events_off.iter().any(|o| o == k),
        }
    }

    /// Turns a highlight row on or off, and saves it.
    pub(crate) fn set_highlight_on(&mut self, key: &'static str, on: bool, cx: &mut Context<Self>) {
        self.set_highlights(
            move |h| match key {
                "mention" => h.mentions = on,
                "first" => h.first_chatters = on,
                "events" => h.events = on,
                "marks" => h.scrollbar_marks = on,
                k => {
                    h.events_off.retain(|o| o != k);
                    if !on {
                        h.events_off.push(k.to_string());
                    }
                }
            },
            cx,
        );
    }

    /// Sets a highlight's color, saves it, and shows it in that row's hex box.
    pub(crate) fn set_color(
        &mut self,
        key: &'static str,
        color: u32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let hex = style::to_hex(color);
        let saved = hex.clone();
        self.set_highlights(
            move |h| match key {
                "mention" => h.mention_color = saved.clone(),
                "first" => h.first_chatter_color = saved.clone(),
                k => {
                    h.event_colors.insert(k.to_string(), saved.clone());
                }
            },
            cx,
        );
        if let Some((_, input)) = self.color_inputs.iter().find(|(k, _)| *k == key) {
            input.update(cx, |s, cx| s.set_value(hex, window, cx));
        }
    }

    /// A row's hex box was confirmed (Enter or leaving it): a valid color is used, anything else
    /// puts the current one back.
    fn on_color_input(
        &mut self,
        key: &'static str,
        input: &Entity<gpui_component::input::InputState>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
            return;
        }
        let typed = input.read(cx).value().to_string();
        let color = style::parse_hex(&typed).unwrap_or_else(|| self.color_of(key));
        self.set_color(key, color, window, cx);
    }

    /// Keeps the names that chatted for the first time, so they aren't first-timers next time.
    fn keep_known_chatters(&mut self) {
        let changes = self.session.take_known_changes();
        if !self.platforms.saves() || (changes.since.is_empty() && changes.ids.is_empty()) {
            return;
        }
        let name = |p: Platform| p.name().to_lowercase();
        let known = rill_os::known::Known {
            since: changes
                .since
                .into_iter()
                .map(|(p, t)| (name(p), t))
                .collect(),
            ids: changes
                .ids
                .into_iter()
                .map(|(p, id)| (name(p), id))
                .collect(),
        };
        if let Err(e) = rill_os::known::append(&known)
            && !self.known_error_shown
        {
            self.known_error_shown = true;
            self.session
                .notice(None, rill_core::Level::Error, &e, now_ms());
        }
    }

    /// Marks along the scroll bar where highlighted lines are, so a sub or mention that scrolled
    /// off while you were away is easy to find.
    fn render_marks(&mut self) -> Option<AnyElement> {
        let h = &self.highlights;
        if !h.scrollbar_marks || !(h.mentions || h.events || h.first_chatters) {
            return None;
        }
        let total = self.session.feed.visible_len();
        if total == 0 {
            return None;
        }
        let (lines, measured) = self.mark_offsets();
        // A line's share of the track, between 1 and 3 px: thin with a long history.
        let track = f32::from(self.feed_bounds.get().size.height);
        let tall = (track / total as f32).clamp(1., 3.);
        // The scroll bar's own numbers, so marks agree with its thumb.
        let viewport = f32::from(self.list.viewport_bounds().size.height);
        let max_scroll = f32::from(self.list.max_offset_for_scrollbar().y);
        let content = max_scroll + viewport;
        let thumb = if content > 0. {
            (viewport / content * track).max(48.).min(track)
        } else {
            track
        };
        // Where the thumb is when the line is mid-screen (or as near as the ends allow), plus
        // where the line then sits on screen, mapped into the thumb: the mark is level with the
        // line whenever it's in view.
        let place = |y: f32| -> f32 {
            let y = if measured > 0. {
                y / measured * content
            } else {
                0.
            };
            if max_scroll <= 0. || viewport <= 0. {
                return if content > 0. {
                    y / content * track
                } else {
                    0.
                };
            }
            let offset = (y - viewport / 2.).clamp(0., max_scroll);
            let on_screen = (y - offset).clamp(0., viewport);
            offset / max_scroll * (track - thumb) + on_screen / viewport * thumb
        };
        // One mark per pixel row, so a busy stretch doesn't draw thousands.
        let mut rows: Vec<(i32, u32)> = Vec::new();
        for &(y, color) in lines.iter() {
            let at = place(y).round() as i32;
            match rows.last() {
                Some((last, _)) if (*last - at).abs() < 2 => {}
                _ => rows.push((at, color)),
            }
        }
        let marks = rows.into_iter().map(|(at, color)| {
            div()
                .absolute()
                .top(px(at as f32 - tall / 2.))
                .left_0()
                .w_full()
                .h(px(tall))
                .rounded_sm()
                .bg(rgb(color))
        });
        // Only a picture: the scroll bar underneath still takes every click and drag.
        Some(
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .right(px(3.))
                .w(px(4.))
                .children(marks)
                .into_any_element(),
        )
    }

    /// Remembers the height of every row the list has measured from the top of the view down,
    /// so marks can be placed by real heights rather than by counting lines.
    fn record_row_heights(&mut self) {
        let top = self.list.logical_scroll_top().item_ix;
        let end = (top + 120).min(self.session.feed.visible_len());
        for ix in top..end {
            if let (Some(b), Some(e)) =
                (self.list.bounds_for_item(ix), self.session.feed.visible(ix))
            {
                self.row_heights.insert(e.seq, f32::from(b.size.height));
            }
        }
        // Forget rows that have left the feed.
        if self.row_heights.len() > self.session.feed.capacity() + 1_000
            && let Some(first) = self.session.feed.entries().next().map(|e| e.seq)
        {
            self.row_heights.retain(|seq, _| *seq >= first);
        }
    }

    /// Each highlighted line's middle, in pixels from the top of the history (real heights where
    /// known, the estimate elsewhere), and the history's whole height. Recomputed a few times a
    /// second at most: it walks every line.
    fn mark_offsets(&mut self) -> (MarkLines, f32) {
        if let Some((at, lines, height)) = &self.mark_cache
            && at.elapsed() < Duration::from_millis(300)
        {
            return (lines.clone(), *height);
        }
        let marks = self.session.feed.marks();
        let mut next = marks.iter().peekable();
        let mut lines = Vec::with_capacity(marks.len());
        let mut y = 0.;
        for ix in 0..self.session.feed.visible_len() {
            let h = self
                .session
                .feed
                .visible(ix)
                .and_then(|e| self.row_heights.get(&e.seq).copied())
                .unwrap_or(self.hint_height);
            while let Some(m) = next.next_if(|m| m.ix == ix) {
                if let Some(color) = self.line_color(m.mention, m.first, m.event) {
                    lines.push((y + h / 2., color));
                }
            }
            y += h;
        }
        let lines = Rc::new(lines);
        self.mark_cache = Some((Instant::now(), lines.clone(), y));
        (lines, y)
    }

    /// Checks for an update shortly after starting, then every six hours, while it's turned on.
    fn update_loop(cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |view, cx| {
            let mut wait = Duration::from_secs(10);
            loop {
                cx.background_executor().timer(wait).await;
                wait = UPDATE_EVERY;
                let go = view.update(cx, |view, cx| {
                    if view.auto_update && !matches!(view.update, UpdateStatus::Ready(_)) {
                        view.check_for_update(cx);
                    }
                });
                if go.is_err() {
                    return;
                }
            }
        })
    }

    /// Looks for a new version and downloads it, off the UI thread.
    pub(crate) fn check_for_update(&mut self, cx: &mut Context<Self>) {
        if matches!(self.update, UpdateStatus::Checking) {
            return;
        }
        self.update = UpdateStatus::Checking;
        cx.notify();
        self.update_task = Some(cx.spawn(async move |view, cx| {
            let checked = cx
                .background_executor()
                .spawn(async { crate::updates::check() })
                .await;
            let _ = view.update(cx, |view, cx| {
                view.update = match checked {
                    Ok(crate::updates::Checked::NotInstalled) => UpdateStatus::NotInstalled,
                    Ok(crate::updates::Checked::UpToDate) => UpdateStatus::UpToDate,
                    Ok(crate::updates::Checked::Ready(ready)) => {
                        let text = format!(
                            "Rill {} is ready: it installs the next time Rill starts, or now with \
                             Update in the header.",
                            ready.version
                        );
                        view.session
                            .notice(None, rill_core::Level::Info, &text, now_ms());
                        view.sync_list();
                        UpdateStatus::Ready(ready)
                    }
                    Err(e) => {
                        tracing::warn!(err = %e, "update check failed");
                        UpdateStatus::Failed(e)
                    }
                };
                cx.notify();
            });
        }));
    }

    /// Installs the downloaded update now: Rill closes and the new version opens.
    pub(crate) fn restart_to_update(&mut self, cx: &mut Context<Self>) {
        if let UpdateStatus::Ready(ready) = &self.update
            && let Err(e) = ready.restart()
        {
            self.session
                .notice(None, rill_core::Level::Error, &e, now_ms());
            self.sync_list();
            cx.notify();
        }
    }

    /// Turns automatic update checks on or off, and saves the choice.
    pub(crate) fn set_auto_update(&mut self, on: bool, cx: &mut Context<Self>) {
        self.auto_update = on;
        if self.platforms.saves()
            && let Err(e) = rill_os::settings::update(|s| s.auto_update = on)
        {
            self.session
                .notice(None, rill_core::Level::Error, &e, now_ms());
            self.sync_list();
        }
        if on && matches!(self.update, UpdateStatus::Idle) {
            self.check_for_update(cx);
        }
        cx.notify();
    }

    /// Sets the text size (emotes, icons and badges follow), and saves it.
    pub(crate) fn set_text_size(&mut self, pct: u32, cx: &mut Context<Self>) {
        let pct = pct.clamp(TEXT_SIZES[0], TEXT_SIZES[TEXT_SIZES.len() - 1]);
        if pct == self.text_size {
            return;
        }
        self.text_size = pct;
        crate::row::set_scale(pct);
        if self.platforms.saves()
            && let Err(e) = rill_os::settings::update(|s| s.text_size = pct)
        {
            self.session
                .notice(None, rill_core::Level::Error, &e, now_ms());
            self.sync_list();
        }
        // Every row changes height.
        self.row_heights.clear();
        self.mark_cache = None;
        self.list.remeasure();
        cx.notify();
    }

    /// Ctrl+= and Ctrl+-: the next size up or down.
    fn step_text_size(&mut self, by: i32, cx: &mut Context<Self>) {
        let at = TEXT_SIZES
            .iter()
            .position(|&s| s >= self.text_size)
            .unwrap_or(1) as i32;
        let next = (at + by).clamp(0, TEXT_SIZES.len() as i32 - 1) as usize;
        self.set_text_size(TEXT_SIZES[next], cx);
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

    /// The pointer moved onto a link, emote or badge, or off one (`None`, which only clears
    /// a card the same `source` showed). A link's preview is fetched after a short pause on it.
    pub(crate) fn set_hover(
        &mut self,
        source: SharedString,
        hover: Option<crate::chat_text::Hover>,
        cx: &mut Context<Self>,
    ) {
        let Some(hover) = hover else {
            if self.hover.as_ref().is_some_and(|(s, _)| *s == source) {
                self.hover = None;
                self.preview_task = None;
                cx.notify();
            }
            return;
        };
        if self.hover.as_ref() == Some(&(source.clone(), hover.clone())) {
            return;
        }
        self.hover = Some((source, hover.clone()));
        cx.notify();
        let crate::chat_text::Hover::Link { url, .. } = hover else {
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
                let still = matches!(
                    &view.hover,
                    Some((_, crate::chat_text::Hover::Link { url: u, .. })) if *u == url
                );
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
            .children(matches!(self.update, UpdateStatus::Ready(_)).then(|| {
                header_toggle(
                    "update",
                    "Update",
                    true,
                    "A new version is ready: restart Rill to install it",
                    cx.listener(|view, _, _, cx| view.restart_to_update(cx)),
                )
            }))
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
        self.refresh_row_hints();
        self.record_row_heights();
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
            .child(style::scrollbar(Scrollbar::vertical(&self.list)))
            // Drawn over the scroll bar, which still takes the mouse.
            .children(self.render_marks())
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
            .children(self.render_hover())
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
        // Text and spacing are in rems: the text size scales them all.
        window.set_rem_size(px(16. * self.text_size as f32 / 100.));
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
            .on_action(cx.listener(|view, _: &focus::TextBigger, _, cx| view.step_text_size(1, cx)))
            .on_action(
                cx.listener(|view, _: &focus::TextSmaller, _, cx| view.step_text_size(-1, cx)),
            )
            .on_action(
                cx.listener(|view, _: &focus::TextNormal, _, cx| view.set_text_size(100, cx)),
            )
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
