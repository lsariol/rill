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

use crate::chat_view::ChatView;
use crate::composer::Replying;
use crate::style;

actions!(target, [TargetNext, TargetPrev, TargetRun, TargetCancel]);

const TARGET_CONTEXT: &str = "TargetPrompt";

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

    /// The card for what the pointer is over: a link's preview, an emote shown bigger with its
    /// name and source, or what a badge means.
    pub(crate) fn render_hover(&self) -> Option<AnyElement> {
        use crate::chat_text::Hover;
        let (_, hover) = self.hover.clone()?;
        match hover {
            Hover::Link { url, at } => self.render_link_preview(url, at),
            Hover::Emote {
                names,
                url,
                size,
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
                            .child(crate::row::emote_source(&url)),
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

    /// A card floating by `at` (window coordinates): below it in the top half of the chat,
    /// above it in the bottom half. `below` is how far down to start when it goes below.
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

    /// The user card, floating over the chat just below (or above) the clicked name. Chat keeps
    /// moving underneath; opened from the keyboard, it sits at the bottom.
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

    /// A moderation button on the user card: runs it and leaves everything where it is: the
    /// card stays open and the chat doesn't jump to the newest message.
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
        // A tall panel (Settings) scrolls, with a scroll bar, rather than pushing the chat off
        // the window. It grows with its content up to 70% of the window.
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
            } else if p == Platform::Rumble && status == ConnectionStatus::Connected {
                SharedString::from(format!(
                    "Reading {} (read-only)",
                    account.clone().unwrap_or_else(|| "chat".into())
                ))
            } else {
                line
            };
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
            let button = if !set_up || read_only_kick || rumble_link.is_some() {
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
                            .child(
                                // For signing in with a browser other than the default one.
                                Button::new(SharedString::from(format!("copy-link-{p}")))
                                    .xsmall()
                                    .outline()
                                    .label("Copy link")
                                    .on_click(move |_, _, cx| {
                                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                            link.to_string(),
                                        ));
                                    }),
                            )
                            .children(code.map(|c| {
                                Button::new(SharedString::from(format!("copy-{p}")))
                                    .xsmall()
                                    .outline()
                                    .label("Copy code")
                                    .on_click(move |_, _, cx| {
                                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                            c.clone(),
                                        ));
                                    })
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
                .child(
                    Switch::new(SharedString::from(format!("use-{p}")))
                        .small()
                        .checked(on)
                        .disabled(only)
                        .accessibility_label(format!("Use {p}"))
                        .when(only, |s| s.tooltip("At least one platform stays on"))
                        .on_click(cx.listener(move |view, on: &bool, _, cx| {
                            view.set_platform_on(p, *on, cx);
                        })),
                )
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
                        Switch::new("show-deleted")
                            .small()
                            .checked(self.show_deleted)
                            .accessibility_label("Show deleted messages")
                            .on_click(cx.listener(|view, on: &bool, _, cx| {
                                view.set_show_deleted(*on, cx);
                            })),
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
                        Switch::new("link-previews")
                            .small()
                            .checked(self.link_previews)
                            .accessibility_label("Link previews")
                            .on_click(cx.listener(|view, on: &bool, _, cx| {
                                view.set_link_previews(*on, cx);
                            })),
                    ),
            )
            .child(self.render_history_choice(cx))
            .child(self.render_highlight_settings(cx))
            .into_any_element()
    }

    /// Highlights: mentions and events, each on or off with its color, and the scroll bar marks.
    fn render_highlight_settings(&self, cx: &mut Context<Self>) -> AnyElement {
        let h = &self.highlights;
        let row = |id: &'static str,
                   title: &'static str,
                   what: &'static str,
                   on: bool,
                   color: Option<String>,
                   cx: &mut Context<Self>| {
            let swatches = color.map(|current| {
                let current = style::parse_hex(&current);
                div()
                    .flex()
                    .gap_1()
                    .children(style::HIGHLIGHT_COLORS.iter().map(|&c| {
                        let picked = current == Some(c);
                        div()
                            .id(SharedString::from(format!("{id}-{c:06x}")))
                            .size(px(16.))
                            .rounded_full()
                            .cursor_pointer()
                            .bg(rgb(c))
                            .border_2()
                            .border_color(rgb(if picked { 0xffffff } else { style::PANEL }))
                            .on_click(cx.listener(move |view, _, _, cx| {
                                let hex = style::to_hex(c);
                                view.set_highlights(
                                    |h| match id {
                                        "mentions" => h.mention_color = hex.clone(),
                                        _ => h.event_color = hex.clone(),
                                    },
                                    cx,
                                );
                            }))
                    }))
            });
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
                        .child(div().text_xs().text_color(rgb(style::FAINT)).child(what))
                        .children(swatches),
                )
                .child(
                    Switch::new(SharedString::from(format!("highlight-{id}")))
                        .small()
                        .checked(on)
                        .accessibility_label(title)
                        .on_click(cx.listener(move |view, on: &bool, _, cx| {
                            let on = *on;
                            view.set_highlights(
                                |h| match id {
                                    "mentions" => h.mentions = on,
                                    "events" => h.events = on,
                                    _ => h.scrollbar_marks = on,
                                },
                                cx,
                            );
                        })),
                )
        };
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(div().pt_2().font_bold().text_sm().child("Highlights"))
            .child(row(
                "mentions",
                "Mentions of you",
                "Messages that name you stand out in this color.",
                h.mentions,
                Some(h.mention_color.clone()),
                cx,
            ))
            .child(row(
                "events",
                "Events",
                "Subscriptions, gifted subs, raids, Rants, redemptions and ad breaks.",
                h.events,
                Some(h.event_color.clone()),
                cx,
            ))
            .child(row(
                "marks",
                "Marks on the scroll bar",
                "Thin marks along the scroll bar show where highlighted lines are, so a sub you missed is easy to scroll back to.",
                h.scrollbar_marks,
                None,
                cx,
            ))
            .into_any_element()
    }

    /// Message history: how many chat lines to keep, with what each choice costs and how long it
    /// lasts at the current pace of chat.
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
        let mut actions: Vec<(SharedString, Plan, bool)> = Vec::new();
        if let Some(id) = card
            .message_id
            .clone()
            .filter(|_| !deleted && can(Action::Delete))
        {
            actions.push((
                "Delete message".into(),
                Plan::Run {
                    label: format!("Delete {name}'s message"),
                    command: Command::Delete { message_id: id },
                },
                false,
            ));
        }
        if can(Action::Timeout) {
            for secs in [60, 600, 3600] {
                actions.push((
                    format!("Timeout {}", format_duration(secs)).into(),
                    Plan::Run {
                        label: format!("Time out {name} for {}", format_duration(secs)),
                        command: Command::Timeout {
                            user: user.clone(),
                            seconds: secs,
                            reason: None,
                        },
                    },
                    false,
                ));
            }
        }
        if can(Action::Ban) {
            actions.push((
                "Ban".into(),
                Plan::Run {
                    label: format!("Ban {name}"),
                    command: Command::Ban {
                        user: user.clone(),
                        reason: None,
                    },
                },
                true,
            ));
        }
        if can(Action::Unban) {
            actions.push((
                "Unban".into(),
                Plan::Run {
                    label: format!("Unban {name}"),
                    command: Command::Unban { user: user.clone() },
                },
                false,
            ));
        }
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
                Button::new("reply")
                    .xsmall()
                    .primary()
                    .label("Reply")
                    .on_click(cx.listener(move |view, _, window, cx| {
                        view.start_reply(replying.clone(), window, cx);
                    }))
            });
        let no_actions =
            actions.is_empty() && reply.is_none() && self.session.connected().contains(&p);
        let buttons = actions
            .into_iter()
            .enumerate()
            .map(|(i, (label, plan, danger))| {
                let b = Button::new(("mod", i)).xsmall().label(label);
                let b = if danger { b.danger() } else { b.outline() };
                b.on_click(cx.listener(move |view, _, window, cx| {
                    view.run_card_action(p, plan.clone(), window, cx);
                }))
            });
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
                            .text_color(style::readable_name(a.color))
                            .child(name.clone()),
                    )
                    .children(login.map(|l| div().text_color(rgb(style::MUTED)).child(l)))
                    .when(!roles.is_empty(), |d| {
                        d.child(
                            div()
                                .text_xs()
                                .text_color(rgb(style::MUTED))
                                .child(roles.join(" · ")),
                        )
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_1()
                    .children(reply)
                    .children(buttons)
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
