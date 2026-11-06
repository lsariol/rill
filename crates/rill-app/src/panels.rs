//! Panels above the composer: the user card, the "which platform?" prompt and the poll form.

use chrono::TimeZone;
use gpui::{
    AnyElement, AppContext, Context, Entity, IntoElement, ParentElement, SharedString, Styled,
    Window, div, prelude::FluentBuilder, px, rgb,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputState, Textarea, TextareaState};
use gpui_component::{Sizable, StyledExt as _};
use rill_core::command::format_duration;
use rill_core::session::Batch;
use rill_core::{Action, Author, Command, ConnectionStatus, Plan, Platform, UserRef, now_ms};

use crate::chat_view::ChatView;
use crate::composer::Replying;
use crate::style;

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
        cx.notify();
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

    pub(crate) fn render_panel(&mut self, _: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let body: AnyElement = match &self.panel {
            Panel::None => return div().into_any_element(),
            Panel::User(card) => self.render_user_card(card.clone(), cx),
            Panel::Target {
                plan,
                prompt,
                choices,
            } => {
                let buttons = choices.iter().map(|p| {
                    let (p, plan) = (*p, plan.clone());
                    Button::new(SharedString::from(format!("target-{p}")))
                        .small()
                        .outline()
                        .child(
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
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(prompt.clone())
                    .child(div().flex().flex_wrap().gap_2().children(buttons))
                    .into_any_element()
            }
            Panel::Poll(form) => self.render_poll(form, cx),
            Panel::Accounts => self.render_accounts(cx),
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
        div()
            .mx_3()
            .mb_1()
            .p_3()
            .rounded_md()
            .bg(rgb(style::PANEL))
            .border_1()
            .border_color(rgb(style::BORDER))
            .flex()
            .gap_2()
            .child(div().flex_1().min_w_0().child(body))
            .child(
                Button::new("close-panel")
                    .xsmall()
                    .ghost()
                    .label("✕")
                    .tooltip("Close (Esc)")
                    .on_click(cx.listener(|view, _, window, cx| view.close_panel(window, cx))),
            )
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
        let rows = Platform::ALL.into_iter().map(|p| {
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
                    ConnectionStatus::Connected | ConnectionStatus::Reconnecting { .. } => Some(
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
            .into_any_element()
    }

    fn render_user_card(&self, card: UserCard, cx: &mut Context<Self>) -> AnyElement {
        let p = card.platform;
        let a = &card.author;
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
        // Their latest messages this session, newest first: context before acting.
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
                    let text: String = m.plain_text().chars().take(120).collect();
                    Some((at, text, e.deleted))
                }
                _ => None,
            })
            .take(5)
            .collect();
        let can = |action| self.session.can(p, action);
        let mut actions: Vec<(SharedString, Plan, bool)> = Vec::new();
        if let Some(id) = card
            .message_id
            .clone()
            .filter(|_| !card.deleted && can(Action::Delete))
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
            .filter(|_| !card.deleted && can(Action::Reply))
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
                    view.run_plan(p, plan.clone(), window, cx);
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
                d.child(div().flex().flex_col().gap_0p5().text_xs().children(
                    history.into_iter().map(|(at, text, deleted)| {
                        div()
                            .flex()
                            .gap_2()
                            .child(div().flex_none().text_color(rgb(style::FAINT)).child(at))
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_color(rgb(style::MUTED))
                                    .when(deleted, |d| d.line_through())
                                    .child(text),
                            )
                    }),
                ))
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
