//! Panels above the composer: the user card, the "which platform?" prompt and the poll form.

use chrono::TimeZone;
use gpui::StyledImage as _;
use gpui::{
    AnyElement, App, AppContext, Context, Entity, InteractiveElement, IntoElement, KeyBinding,
    ParentElement, SharedString, StatefulInteractiveElement, Styled, Window, actions, div,
    prelude::FluentBuilder, px, rgb,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputState, Textarea, TextareaState};
use gpui_component::switch::Switch;
use gpui_component::{Disableable, Sizable, StyledExt as _};
use rill_core::command::format_duration;
use rill_core::session::Batch;
use rill_core::{Action, Author, Command, ConnectionStatus, Plan, Platform, UserRef, now_ms};
use rill_os::settings::NameStyle;

use crate::chat_view::ChatView;
use crate::composer::Replying;
use crate::style;

actions!(target, [TargetNext, TargetPrev, TargetRun, TargetCancel]);

const TARGET_CONTEXT: &str = "TargetPrompt";

/// YouTube's Terms of Service, which YouTube asks apps using its API to link.
const YOUTUBE_TERMS: &str = "https://www.youtube.com/t/terms";

/// Keys for the "which platform?" prompt: Tab or the arrows pick, Enter or Space runs, Esc cancels.
pub fn init(cx: &mut App) {
    let prompt = Some(TARGET_CONTEXT);
    cx.bind_keys([
        KeyBinding::new("tab", TargetNext, prompt),
        KeyBinding::new("right", TargetNext, prompt),
        KeyBinding::new("down", TargetNext, prompt),
        KeyBinding::new("shift-tab", TargetPrev, prompt),
        KeyBinding::new("left", TargetPrev, prompt),
        KeyBinding::new("up", TargetPrev, prompt),
        KeyBinding::new("enter", TargetRun, prompt),
        KeyBinding::new("space", TargetRun, prompt),
        KeyBinding::new("escape", TargetCancel, prompt),
    ]);
}

/// A count with thousands separators: 20,000.
fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// A file size as KB or MB.
fn file_size(bytes: u64) -> String {
    if bytes < 1024 * 1024 {
        format!("{} KB", bytes.div_ceil(1024))
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

/// Twitch's poll limits, the strictest of the platforms.
const POLL_TITLE_MAX: usize = 60;
const POLL_CHOICE_MAX: usize = 25;
const POLL_DURATIONS: [u32; 4] = [60, 120, 300, 600];

/// What the panel above the composer shows.
#[derive(Default)]
pub enum Panel {
    #[default]
    None,
    User(UserCard),
    Target {
        plan: Plan,
        prompt: String,
        choices: Vec<Platform>,
    },
    Poll(PollForm),
    Accounts,
    Settings,
    /// A batch (such as a nuke) waiting for the user to confirm it.
    Confirm {
        prompt: String,
        batch: Batch,
    },
}

/// A clicked user, with the message they were clicked on.
#[derive(Debug, Clone)]
pub struct UserCard {
    pub platform: Platform,
    pub author: Author,
    pub message_id: Option<String>,
    pub deleted: bool,
    /// The clicked message's text, quoted when replying.
    pub text: String,
}

/// The poll form's fields.
pub struct PollForm {
    platform: Platform,
    title: Entity<InputState>,
    choices: Entity<TextareaState>,
    seconds: u32,
    error: Option<SharedString>,
}

impl ChatView {
    pub(crate) fn open_panel(&mut self, panel: Panel, _: &mut Window, cx: &mut Context<Self>) {
        self.panel = panel;
        self.card_at = None;
        cx.notify();
    }

    /// The card for what the pointer is over: a link preview, a bigger emote, or a badge.
    pub(crate) fn render_hover(&self) -> Option<AnyElement> {
        use crate::chat_text::Hover;
        let (_, hover) = self.hover.clone()?;
        match hover {
            Hover::Link { url, at } => self.render_link_preview(url, at),
            Hover::Emote {
                names,
                url,
                size,
                scope,
                at,
            } => {
                // About three times the size in chat, at most 112 px tall.
                let s = crate::row::emote_size(size);
                let scale = (112. / f32::from(s.height)).min(3.);
                let (w, h) = (s.width * scale, s.height * scale);
                let small = url.clone();
                let image = gpui::img(crate::row::bigger_image(&url))
                    .w(w)
                    .h(h)
                    .with_fallback(move || gpui::img(small.clone()).w(w).h(h).into_any_element());
                let (first, rest) = names.split_first()?;
                let body = div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_1()
                    .child(image)
                    .child(div().font_bold().child(first.clone()))
                    .children((!rest.is_empty()).then(|| {
                        div()
                            .text_xs()
                            .text_color(rgb(style::MUTED))
                            .child(format!("with {}", rest.join(", ")))
                    }))
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(style::FAINT))
                            .child(crate::row::emote_source(&url, scope)),
                    );
                Some(self.hover_frame(at, f32::from(w).max(120.) + 24., 24., body))
            }
            Hover::Badge { title, url, at } => {
                let body = div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .children(url.map(|u| {
                        let small = u.clone();
                        gpui::img(crate::row::bigger_image(&u))
                            .size(px(36.))
                            .with_fallback(move || {
                                gpui::img(small.clone()).size(px(36.)).into_any_element()
                            })
                    }))
                    .child(div().font_bold().child(title));
                Some(self.hover_frame(at, 240., 18., body))
            }
        }
    }

    /// A card floating by `at`: below it in the chat's top half, above it in the bottom half.
    fn hover_frame(
        &self,
        at: gpui::Point<gpui::Pixels>,
        width: f32,
        below: f32,
        body: gpui::Div,
    ) -> AnyElement {
        let feed = self.feed_bounds.get();
        let (w, h) = (f32::from(feed.size.width), f32::from(feed.size.height));
        let x = f32::from(at.x - feed.origin.x).clamp(8., (w - width - 8.).max(8.));
        let y = f32::from(at.y - feed.origin.y);
        let frame = div()
            .absolute()
            .left(px(x))
            .p_2()
            .rounded_md()
            .bg(rgb(style::RAISED))
            .border_1()
            .border_color(rgb(style::BORDER))
            .shadow_lg()
            .child(body);
        if y < h / 2. {
            frame.top(px(y + below)).into_any_element()
        } else {
            frame.bottom(px(h - y + 6.)).into_any_element()
        }
    }

    /// The hovered link's preview (or just its address), floating by the link.
    fn render_link_preview(
        &self,
        url: String,
        at: gpui::Point<gpui::Pixels>,
    ) -> Option<AnyElement> {
        use crate::preview::State;
        let feed = self.feed_bounds.get();
        let (w, h) = (f32::from(feed.size.width), f32::from(feed.size.height));
        let width = 340f32.min(w - 16.);
        let x = f32::from(at.x - feed.origin.x).clamp(8., (w - width - 8.).max(8.));
        let y = f32::from(at.y - feed.origin.y);
        let address = div()
            .text_xs()
            .text_color(rgb(style::FAINT))
            .truncate()
            .child(url.clone());
        let state = if self.link_previews && crate::preview::may_fetch(&url) {
            self.previews.get(&url).cloned().or(Some(State::Loading))
        } else {
            None
        };
        let body = match state {
            Some(State::Ready(p)) => div()
                .flex()
                .flex_col()
                .gap_1()
                .children(p.site.map(|s| {
                    div()
                        .text_xs()
                        .text_color(rgb(style::MUTED))
                        .truncate()
                        .child(s)
                }))
                .children(p.title.map(|t| div().font_bold().line_clamp(2).child(t)))
                .children(p.description.map(|d| {
                    div()
                        .text_xs()
                        .text_color(rgb(style::MUTED))
                        .line_clamp(3)
                        .child(d)
                }))
                .children(p.image.map(|src| {
                    gpui::StyledImage::object_fit(gpui::img(src), gpui::ObjectFit::Contain)
                        .w_full()
                        .max_h(px(170.))
                        .rounded_sm()
                }))
                .child(address),
            Some(State::Loading) => div().flex().flex_col().gap_1().child(address).child(
                div()
                    .text_xs()
                    .text_color(rgb(style::FAINT))
                    .child("Loading preview…"),
            ),
            Some(State::Failed) | None => div().child(address),
        };
        let frame = div()
            .absolute()
            .left(px(x))
            .w(px(width))
            .p_2()
            .rounded_md()
            .bg(rgb(style::RAISED))
            .border_1()
            .border_color(rgb(style::BORDER))
            .shadow_lg()
            .child(body);
        let frame = if y < h / 2. {
            frame.top(px(y + 22.))
        } else {
            frame.bottom(px(h - y + 6.))
        };
        Some(frame.into_any_element())
    }

    /// The user card, by the clicked name, or at the bottom when opened from the keyboard.
    pub(crate) fn render_card(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let Panel::User(card) = &self.panel else {
            return None;
        };
        let feed = self.feed_bounds.get();
        let height = f32::from(feed.size.height);
        let y = self
            .card_at
            .map(|y| f32::from(y - feed.origin.y))
            .unwrap_or(height);
        let below = y < height / 2.;
        let room = if below { height - y - 20. } else { y - 12. };
        let frame = div()
            .id("user-card")
            .absolute()
            .left_3()
            .right_3()
            .max_w(px(560.))
            .max_h(px(room.max(120.)))
            .overflow_y_scroll()
            .p_3()
            .rounded_md()
            .bg(rgb(style::PANEL))
            .border_1()
            .border_color(rgb(style::BORDER))
            .shadow_lg()
            // It floats over the messages: clicks stop here, scrolling still passes.
            .block_mouse_except_scroll()
            // A press anywhere else closes it (a name pressed opens that name's card instead).
            .on_mouse_down_out(cx.listener(|view, _, _, cx| {
                if matches!(view.panel, Panel::User(_)) {
                    view.panel = Panel::None;
                    cx.notify();
                }
            }))
            .flex()
            .gap_2()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(self.render_user_card(card.clone(), cx)),
            )
            .child(
                Button::new("close-card")
                    .xsmall()
                    .ghost()
                    .label("✕")
                    .tooltip("Close (Esc)")
                    .on_click(cx.listener(|view, _, window, cx| view.close_panel(window, cx))),
            );
        let frame = if below {
            frame.top(px(y + 12.))
        } else {
            frame.bottom(px((height - y + 12.).max(8.)))
        };
        Some(frame.into_any_element())
    }

    pub(crate) fn open_poll(
        &mut self,
        platform: Platform,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let title = cx.new(|cx| InputState::new(window, cx).placeholder("Question"));
        let choices = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(2, 5)
                .placeholder("One choice per line, 2 to 5")
        });
        title.update(cx, |s, cx| s.focus(window, cx));
        self.panel = Panel::Poll(PollForm {
            platform,
            title,
            choices,
            seconds: 120,
            error: None,
        });
        cx.notify();
    }

    pub(crate) fn close_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.panel = Panel::None;
        self.input.update(cx, |s, cx| s.focus(window, cx));
        cx.notify();
    }

    /// Runs a planned command on one platform and closes the panel.
    fn move_target(&mut self, by: isize, cx: &mut Context<Self>) {
        if let Panel::Target { choices, .. } = &self.panel
            && !choices.is_empty()
        {
            let n = choices.len() as isize;
            self.target_ix = (self.target_ix as isize + by).rem_euclid(n) as usize;
            cx.notify();
        }
    }

    fn on_target_next(&mut self, _: &TargetNext, _: &mut Window, cx: &mut Context<Self>) {
        self.move_target(1, cx);
    }

    fn on_target_prev(&mut self, _: &TargetPrev, _: &mut Window, cx: &mut Context<Self>) {
        self.move_target(-1, cx);
    }

    fn on_target_run(&mut self, _: &TargetRun, window: &mut Window, cx: &mut Context<Self>) {
        if let Panel::Target { plan, choices, .. } = &self.panel
            && let Some(p) = choices.get(self.target_ix).copied()
        {
            let plan = plan.clone();
            self.run_plan(p, plan, window, cx);
        }
    }

    fn on_target_cancel(&mut self, _: &TargetCancel, window: &mut Window, cx: &mut Context<Self>) {
        self.panel = Panel::None;
        self.focus_composer(window, cx);
    }

    /// Runs a user card's moderation button, keeping the card open and the chat where it is.
    fn run_card_action(
        &mut self,
        platform: Platform,
        plan: Plan,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.session.execute(platform, plan) {
            rill_core::Submitted::Send(out) => {
                self.dispatch(out);
                self.sync_list();
                cx.notify();
            }
            other => {
                self.handle_submitted(other, window, cx);
            }
        }
    }

    fn run_plan(
        &mut self,
        platform: Platform,
        plan: Plan,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.panel = Panel::None;
        let submitted = self.session.execute(platform, plan);
        self.handle_submitted(submitted, window, cx);
        if matches!(self.panel, Panel::None) {
            self.input.update(cx, |s, cx| s.focus(window, cx));
        }
    }

    /// Runs the batch waiting in the confirm panel. Enter in an empty composer does this too.
    pub(crate) fn confirm_batch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Panel::Confirm { batch, .. } = std::mem::take(&mut self.panel) else {
            return;
        };
        let submitted = self.session.run_batch(batch, now_ms());
        self.handle_submitted(submitted, window, cx);
        self.input.update(cx, |s, cx| s.focus(window, cx));
    }

    fn submit_poll(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Panel::Poll(form) = &mut self.panel else {
            return;
        };
        let title = form.title.read(cx).value().trim().to_string();
        let choices: Vec<String> = form
            .choices
            .read(cx)
            .value()
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect();
        let error = if title.is_empty() || title.chars().count() > POLL_TITLE_MAX {
            Some(format!(
                "The question needs 1 to {POLL_TITLE_MAX} characters."
            ))
        } else if !(2..=5).contains(&choices.len()) {
            Some("A poll needs 2 to 5 choices, one per line.".to_string())
        } else if choices.iter().any(|c| c.chars().count() > POLL_CHOICE_MAX) {
            Some(format!(
                "Each choice can have at most {POLL_CHOICE_MAX} characters."
            ))
        } else {
            None
        };
        if let Some(e) = error {
            form.error = Some(e.into());
            cx.notify();
            return;
        }
        let platform = form.platform;
        let seconds = form.seconds;
        let plan = Plan::Run {
            label: format!("Poll \"{title}\""),
            command: Command::Poll {
                title,
                choices,
                seconds,
            },
        };
        self.run_plan(platform, plan, window, cx);
    }

    pub(crate) fn render_panel(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let body: AnyElement = match &self.panel {
            Panel::None => return div().into_any_element(),
            // Floats over the chat instead: see render_card.
            Panel::User(_) => return div().into_any_element(),
            Panel::Target {
                plan,
                prompt,
                choices,
            } => {
                let focused = self.target_focus.contains_focused(window, cx);
                let picked = self.target_ix;
                let buttons = choices.iter().enumerate().map(|(i, p)| {
                    let (p, plan) = (*p, plan.clone());
                    let b = Button::new(SharedString::from(format!("target-{p}"))).small();
                    // The keyboard's pick is filled in; the others are outlines.
                    let b = if focused && i == picked {
                        b.primary()
                    } else {
                        b.outline()
                    };
                    b.child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(style::platform_icon(p, 14.))
                            .child(p.name()),
                    )
                    .on_click(cx.listener(move |view, _, window, cx| {
                        view.run_plan(p, plan.clone(), window, cx);
                    }))
                });
                div()
                    .key_context(TARGET_CONTEXT)
                    .track_focus(&self.target_focus)
                    .on_action(cx.listener(Self::on_target_next))
                    .on_action(cx.listener(Self::on_target_prev))
                    .on_action(cx.listener(Self::on_target_run))
                    .on_action(cx.listener(Self::on_target_cancel))
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(prompt.clone())
                    .child(div().flex().flex_wrap().gap_2().children(buttons))
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(style::FAINT))
                            .child("Tab or arrows pick · Enter runs · Esc cancels"),
                    )
                    .into_any_element()
            }
            Panel::Poll(form) => self.render_poll(form, cx),
            Panel::Accounts => self.render_accounts(cx),
            Panel::Settings => self.render_settings(cx),
            Panel::Confirm { prompt, .. } => div()
                .flex()
                .flex_col()
                .gap_2()
                .child(prompt.clone())
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            Button::new("confirm-batch")
                                .small()
                                .danger()
                                .label("Confirm")
                                .on_click(cx.listener(|view, _, window, cx| {
                                    view.confirm_batch(window, cx)
                                })),
                        )
                        .child(
                            Button::new("cancel-batch")
                                .small()
                                .outline()
                                .label("Cancel")
                                .on_click(
                                    cx.listener(|view, _, window, cx| view.close_panel(window, cx)),
                                ),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(rgb(style::FAINT))
                                .child("Enter confirms · Esc cancels"),
                        ),
                )
                .into_any_element(),
        };
        // A tall panel scrolls, up to 70% of the window, rather than pushing chat off.
        let max = window.viewport_size().height * 0.7;
        // Room on the right for the close button, which stays in the corner while this scrolls.
        let content = div().p_3().pr(px(44.)).child(body);
        let close = div().absolute().top_2().right(px(20.)).child(
            Button::new("close-panel")
                .xsmall()
                .ghost()
                .label("✕")
                .tooltip("Close (Esc)")
                .on_click(cx.listener(|view, _, window, cx| view.close_panel(window, cx))),
        );
        div()
            .relative()
            .mx_3()
            .mb_1()
            .rounded_md()
            .bg(rgb(style::PANEL))
            .border_1()
            .border_color(rgb(style::BORDER))
            .child(
                div()
                    .id("panel")
                    .max_h(max)
                    .overflow_y_scroll()
                    .track_scroll(&self.panel_scroll)
                    .child(content),
            )
            .child(style::scrollbar(
                gpui_component::scroll::Scrollbar::vertical(&self.panel_scroll),
            ))
            .child(close)
            .into_any_element()
    }

    fn account(
        &mut self,
        p: Platform,
        command: Command,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let label = match &command {
            Command::SignOut => format!("Signed out of {p}"),
            // Rumble's "channel" is a private link: never echo it.
            Command::Join { .. } if p == Platform::Rumble => format!("Connected {p}"),
            Command::Join { channel } => format!("Joined {p} channel \"{}\"", channel.trim()),
            _ => format!("Signed in to {p}"),
        };
        let submitted = self.session.execute(p, Plan::Run { command, label });
        self.handle_submitted(submitted, window, cx);
        self.panel = Panel::Accounts;
    }

    fn render_accounts(&self, cx: &mut Context<Self>) -> AnyElement {
        let rows = self.platforms.on().into_iter().map(|p| {
            let link = self.session.link(p);
            let status = link.map(|l| l.status.clone()).unwrap_or_default();
            let account = link
                .and_then(|l| l.account.as_ref())
                .map(|a| a.display_name.clone());
            let prompt = link.and_then(|l| l.sign_in.clone());
            let set_up = self.hub.has(p);
            let line: SharedString = match (&status, &account) {
                _ if !set_up => "Coming soon".into(),
                (ConnectionStatus::Connected, Some(a)) => format!("Connected as {a}").into(),
                (ConnectionStatus::Waiting(why), Some(a)) => {
                    let why = why.strip_prefix("Signed in. ").unwrap_or(why);
                    format!("Signed in as {a}. {why}").into()
                }
                (ConnectionStatus::Reconnecting { .. }, _) => status.label().into(),
                (ConnectionStatus::Connecting, _) if prompt.is_none() => "Connecting…".into(),
                (ConnectionStatus::Connecting, _) => "Waiting for you to approve".into(),
                _ => "Not connected".into(),
            };
            let error = match &status {
                ConnectionStatus::Failed(why) => Some(why.clone()),
                _ => None,
            };
            let read_only_kick = p == Platform::Kick
                && set_up
                && !matches!(
                    status,
                    ConnectionStatus::Connected
                        | ConnectionStatus::Reconnecting { .. }
                        | ConnectionStatus::Connecting
                );
            let kick_join = read_only_kick.then(|| {
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .mt_1()
                    .child(field(Input::new(&self.kick_channel).xsmall()))
                    .child(
                        Button::new("kick-join")
                            .xsmall()
                            .primary()
                            .label("Read chat")
                            .on_click(cx.listener(|view, _, window, cx| {
                                let channel = view.kick_channel.read(cx).value().to_string();
                                view.account(Platform::Kick, Command::Join { channel }, window, cx);
                            })),
                    )
            });
            let line = if p == Platform::Kick
                && status == ConnectionStatus::Connected
                && account.is_none()
            {
                SharedString::from("Reading chat (read-only)")
            } else if let (true, Some(a)) = (read_only_kick, &account) {
                SharedString::from(format!("Signed in as {a}. Pick a channel to read."))
            } else if p == Platform::Rumble && status == ConnectionStatus::Connected {
                SharedString::from(format!(
                    "Reading {} (read-only)",
                    account.clone().unwrap_or_else(|| "chat".into())
                ))
            } else {
                line
            };
            let youtube_limited = (p == Platform::YouTube && rill_youtube::LIMITED).then(|| {
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .text_color(rgb(style::MUTED))
                            .child(div().text_color(rgb(style::WARN)).child("Limited for now."))
                            .child(
                                "Until Google finishes reviewing Rill, only Google accounts on \
                                 Rill's test list can connect, and you sign in again every 7 days.",
                            ),
                    )
                    .child(
                        Button::new("youtube-limited")
                            .xsmall()
                            .ghost()
                            .label("Details")
                            .on_click(|_, _, cx| {
                                cx.open_url(&format!("{}/support#youtube", rill_core::WEBSITE))
                            }),
                    )
            });
            // YouTube's rules: before connecting, people agree to its terms and Rill's privacy policy.
            let youtube_terms =
                (p == Platform::YouTube && set_up && account.is_none()).then(|| {
                    let open = |id: &'static str, label: &'static str, url: String| {
                        Button::new(id)
                            .xsmall()
                            .ghost()
                            .label(label)
                            .on_click(move |_, _, cx| cx.open_url(&url))
                    };
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap_1()
                        .child(
                            div()
                                .text_xs()
                                .text_color(rgb(style::MUTED))
                                .child("Connecting YouTube means you agree to:"),
                        )
                        .child(open(
                            "youtube-terms",
                            "YouTube's Terms of Service",
                            YOUTUBE_TERMS.into(),
                        ))
                        .child(open(
                            "youtube-privacy",
                            "Rill's privacy policy",
                            format!("{}/privacy", rill_core::WEBSITE),
                        ))
                });
            // Rumble connects with the private link from the account's Live Stream API page.
            let rumble_link = (p == Platform::Rumble
                && set_up
                && !matches!(
                    status,
                    ConnectionStatus::Connected
                        | ConnectionStatus::Reconnecting { .. }
                        | ConnectionStatus::Connecting
                ))
            .then(|| {
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .mt_1()
                    .child(
                        div()
                            .text_xs()
                            .font_bold()
                            .text_color(rgb(style::MUTED))
                            .child("Live Stream API link (not your stream key)"),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(field(Input::new(&self.rumble_link).xsmall()))
                            .child(
                                Button::new("rumble-connect")
                                    .xsmall()
                                    .primary()
                                    .label("Connect")
                                    .on_click(cx.listener(|view, _, window, cx| {
                                        let link = view.rumble_link.read(cx).value().to_string();
                                        view.rumble_link
                                            .update(cx, |s, cx| s.set_value("", window, cx));
                                        view.account(
                                            Platform::Rumble,
                                            Command::Join { channel: link },
                                            window,
                                            cx,
                                        );
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .text_xs()
                            .text_color(rgb(style::FAINT))
                            .child("Copy it from rumble.com/account/livestream-api.")
                            .child(
                                Button::new("rumble-page")
                                    .xsmall()
                                    .ghost()
                                    .label("Open page")
                                    .on_click(|_, _, cx| {
                                        cx.open_url("https://rumble.com/account/livestream-api")
                                    }),
                            ),
                    )
            });
            // Kick reads without an account; signing in adds sending and moderation.
            let kick_sign_in =
                (p == Platform::Kick && set_up && account.is_none() && prompt.is_none()).then(
                    || {
                        Button::new("kick-signin")
                            .xsmall()
                            .primary()
                            .label("Sign in")
                            .tooltip("Sign in to send and moderate on Kick")
                            .on_click(cx.listener(|view, _, window, cx| {
                                view.account(Platform::Kick, Command::SignIn, window, cx);
                            }))
                    },
                );
            // Signed in to Kick without a channel open: signing out is still offered.
            let kick_signed_idle = read_only_kick && account.is_some();
            let button = if kick_signed_idle {
                Some(
                    Button::new("signout-kick-idle")
                        .xsmall()
                        .outline()
                        .label("Disconnect")
                        .on_click(cx.listener(move |view, _, window, cx| {
                            view.account(Platform::Kick, Command::SignOut, window, cx);
                        })),
                )
            } else if !set_up || read_only_kick || rumble_link.is_some() {
                None
            } else {
                match status {
                    ConnectionStatus::Connected
                    | ConnectionStatus::Reconnecting { .. }
                    | ConnectionStatus::Waiting(_) => Some(
                        Button::new(SharedString::from(format!("signout-{p}")))
                            .xsmall()
                            .outline()
                            .label("Disconnect")
                            .on_click(cx.listener(move |view, _, window, cx| {
                                view.account(p, Command::SignOut, window, cx);
                            })),
                    ),
                    ConnectionStatus::Connecting if prompt.is_some() => Some(
                        Button::new(SharedString::from(format!("cancel-{p}")))
                            .xsmall()
                            .ghost()
                            .label("Cancel")
                            .on_click(cx.listener(move |view, _, window, cx| {
                                view.account(p, Command::SignOut, window, cx);
                            })),
                    ),
                    ConnectionStatus::Connecting => None,
                    _ => Some(
                        Button::new(SharedString::from(format!("signin-{p}")))
                            .xsmall()
                            .primary()
                            .label("Connect")
                            .on_click(cx.listener(move |view, _, window, cx| {
                                view.account(p, Command::SignIn, window, cx);
                            })),
                    ),
                }
            };
            let prompt_box = prompt.map(|pr| {
                let url = pr.url.clone();
                let link = pr.url.clone();
                let code = pr.code.clone();
                let short = url
                    .trim_start_matches("https://")
                    .split('?')
                    .next()
                    .unwrap_or_default()
                    .to_string();
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .mt_1()
                    .p_2()
                    .rounded_md()
                    .bg(rgb(style::RAISED))
                    .child(match &code {
                        Some(_) => format!("Open {short} and enter this code:"),
                        None => format!("Finish signing in at {short}."),
                    })
                    .children(code.clone().map(|c| div().text_xl().font_bold().child(c)))
                    .child(
                        div()
                            .flex()
                            .gap_1()
                            .child(
                                Button::new(SharedString::from(format!("open-{p}")))
                                    .xsmall()
                                    .primary()
                                    .label("Open page")
                                    .on_click(move |_, _, cx| cx.open_url(&url)),
                            )
                            // For signing in with a browser other than the default one.
                            .child(self.copy_button(
                                format!("copy-link-{p}"),
                                "Copy link",
                                link.to_string(),
                                cx,
                            ))
                            .children(code.map(|c| {
                                self.copy_button(
                                    format!("copy-{p}"),
                                    "Copy code",
                                    c.to_string(),
                                    cx,
                                )
                            })),
                    )
            });
            div()
                .flex()
                .flex_col()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(style::platform_icon(p, 16.))
                        .child(div().w(px(64.)).child(p.name()))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_color(rgb(style::MUTED))
                                .child(line),
                        )
                        .children(kick_sign_in)
                        .children(button),
                )
                .children(error.map(|e| div().text_xs().text_color(rgb(style::ERROR)).child(e)))
                .children(youtube_limited)
                .children(youtube_terms)
                .children(prompt_box)
                .children(kick_join)
                .children(rumble_link)
        });
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(div().font_bold().child("Accounts"))
            .children(rows)
            .children((self.platforms.on().len() < Platform::ALL.len()).then(|| {
                div()
                    .text_xs()
                    .text_color(rgb(style::FAINT))
                    .child("Platforms turned off in Settings aren't listed.")
            }))
            .into_any_element()
    }

    fn render_settings(&self, cx: &mut Context<Self>) -> AnyElement {
        let last = self.platforms.on();
        let rows = Platform::ALL.into_iter().map(|p| {
            let on = self.platforms.is_on(p);
            let what = match p {
                Platform::Twitch | Platform::Kick => "Chat, moderation and channel tools",
                Platform::YouTube if rill_youtube::LIMITED => {
                    "Live chat and moderation (limited for now)"
                }
                Platform::YouTube => "Live chat and moderation",
                Platform::Rumble => "Read-only chat and Rants",
            };
            let only = on && last == [p];
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(style::platform_icon(p, 16.))
                .child(div().w(px(64.)).child(p.name()))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_color(rgb(style::MUTED))
                        .child(what),
                )
                .child(style::switch(
                    Switch::new(SharedString::from(format!("use-{p}")))
                        .small()
                        .checked(on)
                        .disabled(only)
                        .accessibility_label(format!("Use {p}"))
                        .when(only, |s| s.tooltip("At least one platform stays on"))
                        .on_click(cx.listener(move |view, on: &bool, _, cx| {
                            view.set_platform_on(p, *on, cx);
                        })),
                ))
        });
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(div().font_bold().child("Settings"))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .child(div().font_bold().text_sm().child("Platforms"))
                    .child(div().text_xs().text_color(rgb(style::FAINT)).child(
                        "A platform you turn off disconnects and disappears from the chat, the header \
                         and Accounts. Its sign-in is kept for when you turn it back on.",
                    )),
            )
            .children(rows)
            .child(
                div()
                    .pt_2()
                    .child(div().font_bold().text_sm().child("Chat")),
            )
            .child(switch_row(
                "show-times",
                "Show times",
                "Each message's time. A narrow window hides times anyway.",
                self.show_times,
                cx.listener(|view, on: &bool, _, cx| {
                    let on = *on;
                    view.set_display(|v| v.show_times = on, |s| s.show_times = on, cx);
                }),
            ))
            .child(self.names_choice(
                "names",
                "Names",
                "Simple colors each name by role and shows no badges but the platform's icon. \
                 Badges shows owner, moderator, VIP and subscriber badges and each name's own \
                 color; a narrow window hides the badges.",
                self.names,
                |view, style, cx| view.set_display(|v| v.names = style, |s| s.names = style, cx),
                cx,
            ))
            .child(self.render_name_colors(cx))
            .child(switch_row(
                "mentions-only",
                "Only messages that mention you",
                "Hide everything else until this is turned off.",
                self.session.feed.filter().mentions_only,
                cx.listener(|view, on: &bool, _, cx| view.set_mentions_only(*on, cx)),
            ))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child("Show deleted messages")
                            .child(div().text_xs().text_color(rgb(style::FAINT)).child(
                                "Keep the text of deleted messages, faded and struck through, \
                                 instead of \"message deleted\".",
                            )),
                    )
                    .child(
                        style::switch(Switch::new("show-deleted")
                            .small()
                            .checked(self.show_deleted)
                            .accessibility_label("Show deleted messages")
                            .on_click(cx.listener(|view, on: &bool, _, cx| {
                                view.set_show_deleted(*on, cx);
                            })),)
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child("Link previews")
                            .child(div().text_xs().text_color(rgb(style::FAINT)).child(
                                "Hovering a link shows the page's title and picture. Rill visits \
                                 the page to get them, so the site sees your IP address; with \
                                 this off, hovering shows only the address.",
                            )),
                    )
                    .child(
                        style::switch(Switch::new("link-previews")
                            .small()
                            .checked(self.link_previews)
                            .accessibility_label("Link previews")
                            .on_click(cx.listener(|view, on: &bool, _, cx| {
                                view.set_link_previews(*on, cx);
                            })),)
                    ),
            )
            .child(self.render_text_size(cx))
            .child(self.render_history_choice(cx))
            .child(self.render_highlight_settings(cx))
            .child(self.render_overlay_settings(cx))
            .child(self.render_updates(cx))
            .child(self.render_about(cx))
            .into_any_element()
    }

    /// Highlight settings: mentions, first-time chatters, events, colors and scroll bar marks.
    fn render_highlight_settings(&self, cx: &mut Context<Self>) -> AnyElement {
        let events_on = self.highlight_on("events");
        let mut rows: Vec<AnyElement> = vec![
            self.highlight_row(
                "mention",
                "Mentions of you",
                "Messages that name you.",
                true,
                false,
                cx,
            ),
            self.highlight_row(
                "first",
                "First-time chatters",
                "Someone's first message in your chat on Twitch, Kick or Rumble.",
                true,
                false,
                cx,
            ),
        ];
        rows.push(self.render_first_chatters_note(cx));
        rows.push(self.highlight_row(
            "events",
            "Events",
            "Subscriptions, gifts, raids and the rest, each in its own color below.",
            false,
            false,
            cx,
        ));
        for kind in rill_core::EventKind::ALL {
            rows.push(self.highlight_row(kind.key(), kind.label(), "", true, !events_on, cx));
        }
        rows.push(self.highlight_row(
            "marks",
            "Marks on the scroll bar",
            "Thin marks along the scroll bar show where highlighted lines are, so a sub you \
             missed is easy to scroll back to.",
            false,
            false,
            cx,
        ));
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(div().pt_2().font_bold().text_sm().child("Highlights"))
            .children(rows)
            .into_any_element()
    }

    /// A Simple or Badges choice for how names look.
    fn names_choice(
        &self,
        id: &'static str,
        title: &'static str,
        what: &'static str,
        current: NameStyle,
        pick: fn(&mut Self, NameStyle, &mut Context<Self>),
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let buttons = [(NameStyle::Simple, "Simple"), (NameStyle::Badges, "Badges")].map(
            |(choice, label)| {
                let b = Button::new((id, choice as usize)).xsmall().label(label);
                let b = if choice == current {
                    b.primary()
                } else {
                    b.outline()
                };
                b.on_click(cx.listener(move |view, _, _, cx| pick(view, choice, cx)))
            },
        );
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(title)
            .child(div().text_xs().text_color(rgb(style::FAINT)).child(what))
            .child(div().flex().gap_1().children(buttons))
            .into_any_element()
    }

    /// Simple view's name colors, one row per role.
    fn render_name_colors(&self, cx: &mut Context<Self>) -> AnyElement {
        let rows = rill_core::Rank::ALL.map(|rank| {
            div()
                .flex()
                .flex_col()
                .gap_1()
                .pl_4()
                .child(rank.label())
                .child(self.color_picker(crate::chat_view::name_key(rank), &style::NAME_COLORS, cx))
        });
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(style::FAINT))
                    .child("Simple's colors, here and on the overlay:"),
            )
            .children(rows)
            .into_any_element()
    }

    /// Color swatches and a hex box for a color setting.
    fn color_picker(
        &self,
        key: &'static str,
        palette: &[u32],
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let current = self.color_of(key);
        let swatches: Vec<_> = palette
            .iter()
            .map(|&c| {
                div()
                    .id(SharedString::from(format!("{key}-{c:06x}")))
                    .size(px(16.))
                    .rounded_full()
                    .cursor_pointer()
                    .bg(rgb(c))
                    .border_2()
                    .border_color(rgb(if c == current { 0xffffff } else { style::PANEL }))
                    // Under the pointer, an unchosen color draws in as a smaller dot.
                    .when(c != current, |d| d.hover(|s| s.border_4()))
                    .on_click(cx.listener(move |view, _, window, cx| {
                        view.set_color(key, c, window, cx);
                    }))
            })
            .collect();
        let hex = self
            .color_inputs
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, input)| div().w(px(84.)).child(Input::new(input).xsmall()));
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_1()
            // The color in use, even when it isn't one of the swatches.
            .child(div().w(px(4.)).h(px(16.)).rounded_sm().bg(rgb(current)))
            .children(swatches)
            .children(hex)
    }

    /// One highlight row: name, switch, and with `colored` its swatches and hex box.
    fn highlight_row(
        &self,
        key: &'static str,
        title: &'static str,
        what: &'static str,
        colored: bool,
        dimmed: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let on = self.highlight_on(key);
        let bare = key.strip_prefix("o.").unwrap_or(key);
        let is_kind = rill_core::EventKind::ALL.iter().any(|k| k.key() == bare);
        let colors = colored.then(|| self.color_picker(key, &style::HIGHLIGHT_COLORS, cx));
        div()
            .flex()
            .items_center()
            .gap_2()
            .when(is_kind, |d| d.pl_4())
            .when(dimmed, |d| d.opacity(0.5))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(title)
                    .when(!what.is_empty(), |d| {
                        d.child(div().text_xs().text_color(rgb(style::FAINT)).child(what))
                    })
                    .children(colors),
            )
            .child(style::switch(
                Switch::new(SharedString::from(format!("highlight-{key}")))
                    .small()
                    .checked(on)
                    .accessibility_label(title)
                    .on_click(cx.listener(move |view, on: &bool, _, cx| {
                        view.set_highlight_on(key, *on, cx);
                    })),
            ))
            .into_any_element()
    }

    /// The OBS overlay: on or off, its link, how to add it to OBS, its look, and its highlights.
    fn render_overlay_settings(&self, cx: &mut Context<Self>) -> AnyElement {
        use rill_os::settings::Overlay;
        let o = &self.overlay_settings;
        let mut col = div()
            .flex()
            .flex_col()
            .gap_2()
            .child(div().pt_2().font_bold().text_sm().child("OBS overlay"))
            .child(self.overlay_switch(
                "overlay-on",
                "Show chat on stream",
                "Rill shows your chat as a page OBS can add as a Browser source. Only this \
                 computer can open it, and it runs only while this is on and Rill is open.",
                o.enabled,
                |o, on| o.enabled = on,
                cx,
            ));
        if !o.enabled {
            return col.into_any_element();
        }
        let faint = |text: String| div().text_xs().text_color(rgb(style::FAINT)).child(text);
        if let Some(e) = &self.overlay_error {
            col = col.child(
                div()
                    .text_xs()
                    .text_color(rgb(style::ERROR))
                    .child(e.clone()),
            );
        } else if let Some(overlay) = &self.overlay {
            let url = overlay.url();
            col = col.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .font_family("monospace")
                            .child(url.clone()),
                    )
                    .child(self.copy_button("overlay-copy", "Copy link", url.to_string(), cx)),
            );
        }
        col = col.child(faint(
            "In OBS, add a Browser source (Sources, +, Browser), paste the link, and set its \
             Width and Height to the space chat should fill, such as 400 by 900. Chat sits at the \
             bottom and older lines move up. To make it bigger later, change Width and Height in \
             the source's properties: dragging its corners only stretches it, so no more lines \
             fit. For a look of your own, add CSS in the source's Custom CSS box."
                .into(),
        ));
        let choices = |label: &'static str,
                       id: &'static str,
                       options: &[(u32, &'static str)],
                       current: u32,
                       set: fn(&mut Overlay, u32),
                       cx: &mut Context<Self>| {
            let buttons: Vec<_> = options
                .iter()
                .map(|&(value, text)| {
                    let b = Button::new((id, value as usize)).xsmall().label(text);
                    let b = if value == current {
                        b.primary()
                    } else {
                        b.outline()
                    };
                    b.on_click(cx.listener(move |view, _, _, cx| {
                        view.set_overlay(move |o| set(o, value), cx);
                    }))
                })
                .collect();
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(label)
                .child(div().flex().flex_wrap().gap_1().children(buttons))
        };
        col = col
            .child(choices(
                "Text size",
                "overlay-font",
                &[(14, "14"), (18, "18"), (22, "22"), (28, "28"), (36, "36")],
                u32::from(o.font_size),
                |o, v| o.font_size = v as u16,
                cx,
            ))
            .child(choices(
                "Lines fade away",
                "overlay-fade",
                &[
                    (0, "Never"),
                    (30, "After 30 s"),
                    (60, "After 1 min"),
                    (120, "After 2 min"),
                ],
                o.fade_seconds,
                |o, v| o.fade_seconds = v,
                cx,
            ))
            .child(self.names_choice(
                "overlay-names",
                "Names",
                "Simple or Badges, as under Chat, for your stream.",
                o.names,
                |view, style, cx| {
                    view.set_overlay(|o| o.names = style, cx);
                    view.reseed_overlay();
                },
                cx,
            ))
            .child(self.overlay_switch(
                "overlay-icons",
                "Platform icons",
                "",
                o.platform_icons,
                |o, on| o.platform_icons = on,
                cx,
            ))
            .child(self.overlay_switch(
                "overlay-commands",
                "Hide commands",
                "Messages that start with !, such as !uptime.",
                o.hide_commands,
                |o, on| o.hide_commands = on,
                cx,
            ))
            .child(self.overlay_switch(
                "overlay-bots",
                "Hide bots",
                "Nightbot, StreamElements, Streamlabs, Moobot, Fossabot, Botrix and other \
                 well-known bots.",
                o.hide_bots,
                |o, on| o.hide_bots = on,
                cx,
            ))
            .child(self.overlay_switch(
                "overlay-background",
                "Dark backing",
                "A dark box behind each line, for busy scenes. Off, lines have a shadow instead.",
                o.background,
                |o, on| o.background = on,
                cx,
            ))
            .child(div().pt_1().child("Overlay highlights"))
            .child(faint(
                "Separate from Rill's own, and all off to start: your stream shows plain chat \
                 unless you turn these on."
                    .into(),
            ))
            .child(self.highlight_row("o.mention", "Mentions of you", "", true, false, cx))
            .child(self.highlight_row(
                "o.first",
                "First-time chatters",
                if self.highlights.first_chatters {
                    ""
                } else {
                    "Needs First-time chatters turned on under Highlights above."
                },
                true,
                false,
                cx,
            ))
            .child(self.highlight_row("o.events", "Events", "", false, false, cx));
        let events_on = self.highlight_on("o.events");
        for kind in rill_core::EventKind::ALL {
            if let Some(key) = crate::chat_view::overlay_key(kind) {
                col = col.child(self.highlight_row(key, kind.label(), "", true, !events_on, cx));
            }
        }
        col.into_any_element()
    }

    /// One on-or-off overlay setting.
    fn overlay_switch(
        &self,
        id: &'static str,
        title: &'static str,
        what: &'static str,
        on: bool,
        set: fn(&mut rill_os::settings::Overlay, bool),
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(title)
                    .when(!what.is_empty(), |d| {
                        d.child(div().text_xs().text_color(rgb(style::FAINT)).child(what))
                    }),
            )
            .child(style::switch(
                Switch::new(id)
                    .small()
                    .checked(on)
                    .accessibility_label(title)
                    .on_click(cx.listener(move |view, on: &bool, _, cx| {
                        let on = *on;
                        view.set_overlay(move |o| set(o, on), cx);
                    })),
            ))
            .into_any_element()
    }

    /// What first-time chatters cost and how far along they are, or a way to delete the old list.
    fn render_first_chatters_note(&self, cx: &mut Context<Self>) -> AnyElement {
        let on = self.highlight_on("first");
        let mut lines: Vec<String> = Vec::new();
        let mut saved = None;
        if on {
            lines.push(format!(
                "Rill keeps a list of everyone who chats in your channels, on this computer only. \
                 It knows {} people so far.",
                thousands(self.session.known_count())
            ));
            lines.extend(self.first_chatters_note());
        } else {
            lines.push(
                "The platforms don't tell apps like Rill who is chatting for the first time, so \
                 Rill keeps its own list of everyone who chats in your channels, on this computer only. It uses \
                 about 80 bytes of memory per person: under 1 MB for a small channel, a few MB \
                 for most, about 40 MB for one with half a million different chatters. Nothing \
                 is marked for the first week, while Rill learns who has chatted before."
                    .into(),
            );
            if self.platforms.saves() {
                saved = rill_os::known::size().filter(|&n| n > 0);
            }
        }
        div()
            .pl_2()
            .flex()
            .flex_col()
            .gap_1()
            .text_xs()
            .text_color(rgb(style::MUTED))
            .children(lines.into_iter().map(|l| div().child(l)))
            .when_some(saved, |d, bytes| {
                d.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(div().flex_1().child(format!(
                            "The list from before is still saved ({}).",
                            file_size(bytes)
                        )))
                        .child(
                            Button::new("delete-known")
                                .xsmall()
                                .outline()
                                .label("Delete it")
                                .on_click(cx.listener(|view, _, _, cx| {
                                    view.delete_known_chatters(cx);
                                })),
                        ),
                )
            })
            .into_any_element()
    }

    /// While Rill is still learning who has chatted, when first-time chatters start being marked.
    fn first_chatters_note(&self) -> Option<String> {
        let now = now_ms();
        let starts: Vec<i64> = self
            .platforms
            .on()
            .into_iter()
            .filter_map(|p| self.session.first_chatters_from(p))
            .collect();
        if starts.is_empty() {
            return Some(
                "Learning starts when chat arrives; first-time chatters are marked a week later."
                    .into(),
            );
        }
        let latest = *starts.iter().max()?;
        (latest > now).then(|| {
            let date = chrono::Local
                .timestamp_millis_opt(latest)
                .single()
                .map(|t| t.format("%B %-d").to_string())
                .unwrap_or_default();
            format!(
                "Rill is learning who has chatted before, so it doesn't mark everyone at first: \
                 first-time chatters are marked from {date}."
            )
        })
    }

    /// Updates: the automatic check, where it stands, and buttons to check or install now.
    fn render_updates(&self, cx: &mut Context<Self>) -> AnyElement {
        use crate::chat_view::UpdateStatus;
        let version = env!("CARGO_PKG_VERSION");
        let status: String = match &self.update {
            UpdateStatus::Idle if self.auto_update => format!("Rill {version}. Checking shortly."),
            UpdateStatus::Idle => format!("Rill {version}."),
            UpdateStatus::Checking => format!("Rill {version}. Checking for a new version…"),
            UpdateStatus::NotInstalled => format!(
                "Rill {version}, run from source: only Rill installed with its installer updates."
            ),
            UpdateStatus::UpToDate => format!("Rill {version} is the newest version."),
            UpdateStatus::Ready(r) => format!(
                "Rill {} is downloaded: it installs the next time Rill starts.",
                r.version
            ),
            UpdateStatus::Failed(why) => why.clone(),
        };
        let ready = matches!(self.update, UpdateStatus::Ready(_));
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(div().pt_2().font_bold().text_sm().child("Updates"))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child("Check for updates automatically")
                            .child(div().text_xs().text_color(rgb(style::FAINT)).child(
                                "Rill looks for a new version when it starts and every six hours, \
                                 downloads it in the background, and installs it the next time \
                                 it starts. It never restarts on its own.",
                            )),
                    )
                    .child(style::switch(
                        Switch::new("auto-update")
                            .small()
                            .checked(self.auto_update)
                            .accessibility_label("Check for updates automatically")
                            .on_click(cx.listener(|view, on: &bool, _, cx| {
                                view.set_auto_update(*on, cx);
                            })),
                    )),
            )
            .child(div().text_xs().text_color(rgb(style::MUTED)).child(status))
            .child(
                div()
                    .flex()
                    .gap_1()
                    .child(
                        Button::new("check-update")
                            .xsmall()
                            .outline()
                            .label("Check now")
                            .disabled(matches!(self.update, UpdateStatus::Checking) || ready)
                            .on_click(cx.listener(|view, _, _, cx| view.check_for_update(cx))),
                    )
                    .when(ready, |d| {
                        d.child(
                            Button::new("restart-update")
                                .xsmall()
                                .primary()
                                .label("Restart to update")
                                .on_click(cx.listener(|view, _, _, cx| view.restart_to_update(cx))),
                        )
                    }),
            )
            .into_any_element()
    }

    /// About: the version and maker, links to the website, and sending logs to the Rill team.
    fn render_about(&self, cx: &mut Context<Self>) -> AnyElement {
        use crate::crash::{Kind, Sending};
        let link = |id: &'static str, label: &'static str, path: &'static str| {
            Button::new(id)
                .xsmall()
                .ghost()
                .label(label)
                .on_click(move |_, _, cx| cx.open_url(&format!("{}{path}", rill_core::WEBSITE)))
        };
        let licences = crate::crash::licences();
        let ours = self.report_kind == Kind::Logs;
        let busy = ours && self.report_sending == Sending::Busy;
        let status = match &self.report_sending {
            Sending::Sent(id) if ours => Some((
                format!("Sent, ID {id}. Email rill@mobasity.com with this ID and what went wrong."),
                style::MUTED,
            )),
            Sending::Failed(e) if ours => Some((e.clone(), style::ERROR)),
            _ => None,
        };
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(div().pt_2().font_bold().text_sm().child("About"))
            .child(format!(
                "Rill {}, made by Mobasity. Free and open source under the MIT license.",
                env!("CARGO_PKG_VERSION")
            ))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_1()
                    .child(link("about-site", "Website", "/"))
                    .child(link("about-support", "Support", "/support"))
                    .child(link("about-privacy", "Privacy", "/privacy"))
                    .child(link("about-terms", "Terms", "/terms"))
                    .when_some(licences, |d, path| {
                        d.child(
                            Button::new("about-licences")
                                .xsmall()
                                .ghost()
                                .label("Open-source licenses")
                                .on_click(move |_, _, cx| cx.open_with_system(&path)),
                        )
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child("Send logs to the Rill team")
                    .child(div().text_xs().text_color(rgb(style::FAINT)).child(
                        "If something isn't working, send Rill's recent log: what it did, never \
                         chat messages or passwords. You'll get an ID to mention when you email \
                         rill@mobasity.com.",
                    )),
            )
            .child(
                div()
                    .flex()
                    .gap_1()
                    .child(
                        Button::new("about-send-logs")
                            .xsmall()
                            .outline()
                            .label(if busy { "Sending…" } else { "Send logs" })
                            .disabled(busy)
                            .on_click(
                                cx.listener(|view, _, _, cx| view.send_report(Kind::Logs, cx)),
                            ),
                    )
                    .child(
                        Button::new("about-open-logs")
                            .xsmall()
                            .ghost()
                            .label("Open logs folder")
                            .on_click(|_, _, cx| cx.open_with_system(&rill_os::log_dir())),
                    ),
            )
            .children(
                status.map(|(text, color)| div().text_xs().text_color(rgb(color)).child(text)),
            )
            .into_any_element()
    }

    /// Text size: a button per size, with the keys that change it.
    fn render_text_size(&self, cx: &mut Context<Self>) -> AnyElement {
        let current = self.text_size;
        let buttons = crate::chat_view::TEXT_SIZES.iter().map(|&pct| {
            let b = Button::new(("text-size", pct as usize))
                .xsmall()
                .label(format!("{pct}%"));
            let b = if pct == current {
                b.primary()
            } else {
                b.outline()
            };
            b.on_click(cx.listener(move |view, _, _, cx| view.set_text_size(pct, cx)))
        });
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child("Text size")
            .child(div().text_xs().text_color(rgb(style::FAINT)).child(
                "Chat, emotes and badges, and the rest of the window. Ctrl+= and Ctrl+- change it \
                 from anywhere, Ctrl+0 puts it back.",
            ))
            .child(div().flex().flex_wrap().gap_1().children(buttons))
            .into_any_element()
    }

    /// Message history: how many lines to keep, with each choice's cost and how long it lasts.
    fn render_history_choice(&self, cx: &mut Context<Self>) -> AnyElement {
        use crate::chat_view::HISTORY_CHOICES;
        let current = self.session.feed.capacity();
        let pace = self.lines_per_minute();
        let lasts = |lines: usize| match pace {
            Some(per_min) => {
                let minutes = lines as f64 / per_min;
                if minutes < 90. {
                    format!(
                        "about {} minutes of chat at the current pace",
                        minutes.round().max(1.)
                    )
                } else if minutes < 48. * 60. {
                    format!(
                        "about {:.0} hours of chat at the current pace",
                        minutes / 60.
                    )
                } else {
                    "days of chat at the current pace".to_string()
                }
            }
            None => "how long it lasts shows once chat is moving".to_string(),
        };
        let buttons = HISTORY_CHOICES.iter().map(|&(lines, note, _)| {
            let b = Button::new(("history", lines))
                .xsmall()
                .label(thousands(lines))
                .tooltip(note);
            let b = if lines == current {
                b.primary()
            } else {
                b.outline()
            };
            b.on_click(cx.listener(move |view, _, _, cx| view.set_history(lines, cx)))
        });
        let detail = match HISTORY_CHOICES.iter().find(|(l, _, _)| *l == current) {
            Some((lines, note, mb)) => format!(
                "{} lines ({note}): Rill uses about {mb} MB in all with it full; {}.",
                thousands(*lines),
                lasts(*lines)
            ),
            None => format!(
                "{} lines (set in settings.json); {}.",
                thousands(current),
                lasts(current)
            ),
        };
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child("Message history")
            .child(div().text_xs().text_color(rgb(style::FAINT)).child(
                "How many chat lines Rill keeps for scrolling back, searching and user cards. \
                 Each 1,000 lines takes about 2 MB; only what's on screen is drawn, so more \
                 history doesn't slow chat down, though searching goes through all of it. A \
                 smaller choice drops the oldest lines at once.",
            ))
            .child(div().flex().flex_wrap().gap_1().children(buttons))
            .child(div().text_xs().text_color(rgb(style::MUTED)).child(detail))
            .into_any_element()
    }

    /// Whether the feed shows this message as deleted.
    fn message_deleted(&self, p: Platform, id: &str) -> bool {
        self.session.feed.entries().any(|e| {
            e.deleted
                && matches!(&e.item, rill_core::Item::Chat(m) if m.platform == p && m.id == id)
        })
    }

    fn render_user_card(&self, card: UserCard, cx: &mut Context<Self>) -> AnyElement {
        let p = card.platform;
        let a = &card.author;
        // Read live: a message deleted while the card is open loses its Delete and Reply.
        let deleted = card.deleted
            || card
                .message_id
                .as_ref()
                .is_some_and(|id| self.message_deleted(p, id));
        let user = UserRef {
            id: Some(a.id.clone()),
            login: a.login.clone(),
        };
        let name = a.display_name.clone();
        let mut roles = Vec::new();
        for (has, label) in [
            (a.roles.broadcaster, "Broadcaster"),
            (a.roles.moderator, "Moderator"),
            (a.roles.vip, "VIP"),
            (a.roles.subscriber, "Subscriber"),
        ] {
            if has {
                roles.push(label);
            }
        }
        let hover = self.link_hover_fn(cx);
        // Everything they said that's still in the feed, newest first: context before acting.
        let history: Vec<_> = self
            .session
            .feed
            .entries()
            .rev()
            .filter_map(|e| match &e.item {
                rill_core::Item::Chat(m) if m.platform == p && m.author.id == a.id => {
                    let at = chrono::Local
                        .timestamp_millis_opt(e.received_ms)
                        .single()
                        .map(|t| t.format("%H:%M").to_string())
                        .unwrap_or_default();
                    Some((
                        e.seq,
                        at,
                        crate::row::history_text(e.seq, m, hover.clone()),
                        e.deleted,
                    ))
                }
                _ => None,
            })
            .collect();
        let said = match history.len() {
            1 => "1 message in the feed".to_string(),
            n => format!("{n} messages in the feed"),
        };
        let can = |action| self.session.can(p, action);
        let run = |label: String, command: Command| Plan::Run { label, command };
        // Unban, the timeouts and Ban in one row, as moderators know it from other chat tools.
        let mut moderation: Vec<gpui::AnyElement> = Vec::new();
        if can(Action::Unban) {
            let plan = run(
                format!("Unban {name}"),
                Command::Unban { user: user.clone() },
            );
            moderation.push(
                mod_button("unban", "Unban", style::OK)
                    .tooltip(|window, cx| {
                        gpui_component::tooltip::Tooltip::new("Lift a ban or a timeout")
                            .build(window, cx)
                    })
                    .on_click(cx.listener(move |view, _, window, cx| {
                        view.run_card_action(p, plan.clone(), window, cx);
                    }))
                    .into_any_element(),
            );
        }
        if can(Action::Timeout) {
            // Kick's shortest timeout is a minute.
            let shortest = if p == Platform::Kick { 60 } else { 1 };
            for secs in CARD_TIMEOUTS.into_iter().filter(|s| *s >= shortest) {
                let label = format_duration(secs);
                let plan = run(
                    format!("Time out {name} for {label}"),
                    Command::Timeout {
                        user: user.clone(),
                        seconds: secs,
                        reason: None,
                    },
                );
                moderation.push(
                    mod_button(("timeout", secs as usize), label, style::TEXT)
                        .on_click(cx.listener(move |view, _, window, cx| {
                            view.run_card_action(p, plan.clone(), window, cx);
                        }))
                        .into_any_element(),
                );
            }
        }
        if can(Action::Ban) {
            let plan = run(
                format!("Ban {name}"),
                Command::Ban {
                    user: user.clone(),
                    reason: None,
                },
            );
            moderation.push(
                mod_button("ban", "Ban", style::ERROR)
                    .on_click(cx.listener(move |view, _, window, cx| {
                        view.run_card_action(p, plan.clone(), window, cx);
                    }))
                    .into_any_element(),
            );
        }
        let delete = card
            .message_id
            .clone()
            .filter(|_| !deleted && can(Action::Delete))
            .map(|id| {
                let plan = run(
                    format!("Delete {name}'s message"),
                    Command::Delete { message_id: id },
                );
                mod_button("delete", "Delete message", style::TEXT).on_click(cx.listener(
                    move |view, _, window, cx| {
                        view.run_card_action(p, plan.clone(), window, cx);
                    },
                ))
            });
        let reply = card
            .message_id
            .clone()
            .filter(|_| !deleted && can(Action::Reply))
            .map(|message_id| {
                let replying = Replying {
                    platform: p,
                    message_id,
                    author: a.display_name.clone(),
                    text: card.text.clone(),
                };
                mod_button("reply", "Reply", style::LINK).on_click(cx.listener(
                    move |view, _, window, cx| {
                        view.start_reply(replying.clone(), window, cx);
                    },
                ))
            });
        let no_actions = moderation.is_empty()
            && reply.is_none()
            && delete.is_none()
            && self.session.connected().contains(&p);
        let login =
            (!a.login.eq_ignore_ascii_case(&a.display_name)).then(|| format!("({})", a.login));
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(style::platform_icon(p, 16.))
                    .child(
                        div()
                            .font_bold()
                            .text_color(self.name_color(Some(a)))
                            .child(name.clone()),
                    )
                    .children(login.map(|l| div().text_color(rgb(style::MUTED)).child(l)))
                    .child(self.copy_button("copy-name", "Copy name", a.display_name.clone(), cx))
                    .when(!roles.is_empty(), |d| {
                        d.child(
                            div()
                                .text_xs()
                                .text_color(rgb(style::MUTED))
                                .child(roles.join(" · ")),
                        )
                    }),
            )
            .when(!a.id.is_empty(), |d| {
                d.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .text_xs()
                        .text_color(rgb(style::MUTED))
                        .child(format!("ID: {}", a.id))
                        .child(self.copy_button("copy-id", "Copy ID", a.id.clone(), cx)),
                )
            })
            .when(!moderation.is_empty(), |d| {
                d.child(div().flex().flex_wrap().gap_1().children(moderation))
            })
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_1()
                    .children(reply)
                    .children(delete)
                    .when(!self.session.connected().contains(&p), |d| {
                        d.child(
                            div().text_xs().text_color(rgb(style::FAINT)).child(format!(
                                "{p} isn't connected, so no actions are available."
                            )),
                        )
                    }),
            )
            .when(!history.is_empty(), |d| {
                d.child(div().text_xs().text_color(rgb(style::FAINT)).child(said))
                    .child(
                        div()
                            .id("history")
                            .max_h(px(220.))
                            .overflow_y_scroll()
                            .flex()
                            .flex_col()
                            .gap_0p5()
                            .text_xs()
                            .children(history.into_iter().map(|(_, at, text, deleted)| {
                                div()
                                    .flex()
                                    .items_start()
                                    .gap_2()
                                    .child(
                                        div()
                                            .flex_none()
                                            .pt(px(1.))
                                            .text_color(rgb(style::FAINT))
                                            .child(at),
                                    )
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .text_color(rgb(style::MUTED))
                                            .when(deleted, |d| d.line_through().opacity(0.6))
                                            .child(text),
                                    )
                            })),
                    )
            })
            .when(no_actions, |d| {
                d.child(div().text_xs().text_color(rgb(style::FAINT)).child(format!(
                    "This {p} account can't moderate here (read-only, or not a moderator)."
                )))
            })
            .into_any_element()
    }

    fn render_poll(&self, form: &PollForm, cx: &mut Context<Self>) -> AnyElement {
        let durations = POLL_DURATIONS.into_iter().map(|secs| {
            let b = Button::new(("poll-secs", secs as usize))
                .xsmall()
                .label(format_duration(secs));
            let b = if secs == form.seconds {
                b.primary()
            } else {
                b.outline()
            };
            b.on_click(cx.listener(move |view, _, _, cx| {
                if let Panel::Poll(form) = &mut view.panel {
                    form.seconds = secs;
                    cx.notify();
                }
            }))
        });
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(style::platform_icon(form.platform, 14.))
                    .child(format!("New poll on {}", form.platform)),
            )
            .child(field(Input::new(&form.title).small()))
            .child(Textarea::new(&form.choices))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(style::MUTED))
                            .mr_1()
                            .child("Length"),
                    )
                    .children(durations)
                    .child(div().flex_1())
                    .child(
                        Button::new("poll-start")
                            .small()
                            .primary()
                            .label("Start poll")
                            .on_click(
                                cx.listener(|view, _, window, cx| view.submit_poll(window, cx)),
                            ),
                    ),
            )
            .when_some(form.error.clone(), |d, e| {
                d.child(div().text_xs().text_color(rgb(style::ERROR)).child(e))
            })
            .min_w(px(0.))
            .into_any_element()
    }
}

/// A text box with a visible frame, so it stands out from the panel behind it.
fn field(input: impl IntoElement) -> gpui::Div {
    div()
        .flex_1()
        .min_w_0()
        .rounded_md()
        .border_1()
        .border_color(rgb(style::FAINT))
        .bg(rgb(style::BACKGROUND))
        .child(input)
}

/// A Settings row: a name and a note on the left, a switch on the right.
fn switch_row(
    id: &'static str,
    name: &'static str,
    note: &'static str,
    checked: bool,
    on_click: impl Fn(&bool, &mut Window, &mut App) + 'static,
) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap_2()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(name)
                .child(div().text_xs().text_color(rgb(style::FAINT)).child(note)),
        )
        .child(style::switch(
            Switch::new(id)
                .small()
                .checked(checked)
                .accessibility_label(name)
                .on_click(on_click),
        ))
}

/// The timeouts the user card offers, in seconds.
const CARD_TIMEOUTS: [u32; 8] = [1, 30, 60, 300, 1800, 3600, 86_400, 604_800];

/// A user card button in `color`, lighter under the pointer and lighter still while pressed.
fn mod_button(
    id: impl Into<gpui::ElementId>,
    label: impl Into<SharedString>,
    color: u32,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .px_2()
        .py_0p5()
        .rounded_sm()
        .border_1()
        .border_color(style::hsla_alpha(color, 0.6))
        .text_xs()
        .text_color(rgb(color))
        .cursor_pointer()
        .hover(|s| s.bg(style::hsla_alpha(color, 0.18)))
        .active(|s| s.bg(style::hsla_alpha(color, 0.38)))
        .child(label.into())
}
