//! Rill: one chronological chat feed for Twitch, YouTube and Kick.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod assets;
mod chat_text;
mod chat_view;
mod composer;
mod crash;
mod focus;
mod image_cache;
mod navigate;
mod panels;
mod platforms;
mod preview;
mod row;
mod search;
mod style;
mod updates;

use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    App, AppContext, Bounds, TitlebarOptions, WindowBounds, WindowOptions, point, px, size,
};
use gpui_component::{Root, Theme, ThemeMode};
use rill_core::{Hub, Platform};
use rill_sim::{Profile, SimAdapter};

use crate::platforms::{MakeAdapter, Platforms};

const USAGE: &str = "\
Usage: rill [options]

Options:
  --sim               Show simulated chat instead of connecting to the platforms
  --rate <n>          Simulated messages per second, per platform (default 4)
  --bursts            Ten times the rate for 3 s every 20 s
  --emotes            Make most words emotes
  --flaky             Drop and restore connections now and then
  --fail <0-1>        Share of sends and actions that fail (default 0.05)
  --history <n>       Keep this many chat lines (default: the Settings choice, 5,000)
  --overlay <port>    Serve the OBS overlay on this port for this run, whatever Settings say
  --exit-after <s>    Quit after this many seconds (for benchmarks)
  --stats <file>      Write counters to this file on quit (with --exit-after)
  --firehose <url>    Load test: real chat from a public firehose (catquery's jsonBasic
                      format), spread across the four platforms; read-only
  -h, --help          Show this help

--rate, --bursts, --emotes, --flaky and --fail apply to simulated chat and turn on --sim.";

/// Command-line options.
#[derive(Debug, Default)]
struct Options {
    sim: bool,
    /// Real chat from a firehose instead of simulated chat.
    firehose: Option<String>,
    profile: Profile,
    exit_after: Option<Duration>,
    stats: Option<std::path::PathBuf>,
    /// Chat lines to keep, overriding Settings.
    history: Option<usize>,
    overlay: Option<u16>,
}

fn parse_args() -> Result<Options, String> {
    let mut o = Options::default();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = |name: &str| {
            args.next()
                .ok_or_else(|| format!("{name} needs a value.\n\n{USAGE}"))
        };
        match arg.as_str() {
            "-h" | "--help" => return Err(USAGE.to_string()),
            "--sim" => o.sim = true,
            "--firehose" => {
                o.firehose = Some(value("--firehose")?);
                o.sim = true;
            }
            "--rate" => {
                o.sim = true;
                o.profile.rate = number(&value("--rate")?, "--rate")?;
            }
            "--fail" => {
                o.sim = true;
                o.profile.fail_rate = number(&value("--fail")?, "--fail")?.clamp(0., 1.);
            }
            "--bursts" => (o.sim, o.profile.bursts) = (true, true),
            "--emotes" => (o.sim, o.profile.emote_heavy) = (true, true),
            "--flaky" => (o.sim, o.profile.flaky) = (true, true),
            "--exit-after" => {
                o.exit_after = Some(Duration::from_secs_f64(number(
                    &value("--exit-after")?,
                    "--exit-after",
                )?))
            }
            "--stats" => o.stats = Some(value("--stats")?.into()),
            "--history" => o.history = Some(number(&value("--history")?, "--history")? as usize),
            "--overlay" => {
                let port = value("--overlay")?;
                o.overlay = Some(port.parse::<u16>().ok().filter(|p| *p > 0).ok_or_else(|| {
                    format!("--overlay needs a port from 1 to 65535, not \"{port}\".")
                })?);
            }
            other => return Err(format!("Unknown option \"{other}\".\n\n{USAGE}")),
        }
    }
    Ok(o)
}

fn number(s: &str, name: &str) -> Result<f64, String> {
    s.parse::<f64>()
        .ok()
        .filter(|n| n.is_finite() && *n >= 0.)
        .ok_or_else(|| format!("{name} needs a number of 0 or more, not \"{s}\"."))
}

/// Logs to daily files in the data folder; returns the guard that flushes them.
fn init_logging() -> Option<tracing_appender::non_blocking::WorkerGuard> {
    let dir = rill_os::log_dir();
    let appender = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("rill")
        .filename_suffix("log")
        .max_log_files(7)
        .build(&dir);
    let appender = match appender {
        Ok(a) => a,
        Err(e) => {
            eprintln!(
                "! Logs: can't write to {}: {e}. Check the folder's permissions; Rill runs without a log.",
                dir.display()
            );
            return None;
        }
    };
    let (writer, guard) = tracing_appender::non_blocking(appender);
    // Debug builds log Rill's own debug lines, not every HTTP/2 frame the libraries see.
    let level = if cfg!(debug_assertions) {
        "info,rill=debug,rill_core=debug,rill_twitch=debug,rill_youtube=debug,rill_kick=debug,rill_rumble=debug,rill_emotes=debug"
    } else {
        "info"
    };
    let filter = tracing_subscriber::EnvFilter::try_from_env("RILL_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(level));
    tracing_subscriber::fmt()
        .with_writer(writer)
        .with_ansi(false)
        .with_env_filter(filter)
        .init();
    Some(guard)
}

/// The real platforms, set up from settings.json, and any problem loading it.
fn real_platforms() -> (Platforms, Vec<String>) {
    let mut errors = Vec::new();
    let settings = rill_os::settings::load().unwrap_or_else(|e| {
        tracing::error!(err = %e, "settings not loaded; using defaults");
        errors.push(e);
        rill_os::settings::Settings::default()
    });
    // Read again for each start, so a platform turned on later uses the current file.
    let make: MakeAdapter = Box::new(|p| {
        let s = rill_os::settings::load().unwrap_or_default();
        match p {
            Platform::Twitch => Box::new(rill_twitch::TwitchAdapter::new(&s.twitch)),
            Platform::YouTube => Box::new(rill_youtube::YouTubeAdapter::new(&s.youtube)),
            Platform::Kick => Box::new(rill_kick::KickAdapter::new(&s.kick)),
            Platform::Rumble => Box::new(rill_rumble::RumbleAdapter::new()),
        }
    });
    let off = Platforms::parse(&settings.disabled_platforms);
    (Platforms::new(off, make, true), errors)
}

fn main() -> ExitCode {
    // Velopack's install and update steps run first, and may end the process.
    updates::hooks();
    let options = match parse_args() {
        Ok(o) => o,
        Err(msg) => {
            let help = msg.starts_with("Usage:");
            if help {
                println!("{msg}");
            } else {
                eprintln!("! {msg}");
            }
            return if help {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            };
        }
    };
    let _log = init_logging();
    crash::install_hook();
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "Rill starting");

    if rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .is_err()
    {
        tracing::warn!("TLS provider was already installed");
    }
    let (platforms, startup_errors) = if let Some(url) = &options.firehose {
        // The firehose takes Twitch's slot; the others connect quietly so every icon is live.
        let quiet = rill_sim::Profile {
            rate: 0.,
            ..options.profile.clone()
        };
        let url = url.clone();
        let make: MakeAdapter = Box::new(move |p| match p {
            Platform::Twitch => Box::new(rill_sim::FirehoseAdapter::new(url.clone())),
            _ => Box::new(SimAdapter::new(p, quiet.clone())),
        });
        (Platforms::new(Default::default(), make, false), Vec::new())
    } else if options.sim {
        let profile = options.profile.clone();
        let make: MakeAdapter = Box::new(move |p| Box::new(SimAdapter::new(p, profile.clone())));
        (Platforms::new(Default::default(), make, false), Vec::new())
    } else {
        real_platforms()
    };
    let (hub, events) = match Hub::start(platforms.start()) {
        Ok(started) => started,
        Err(e) => {
            tracing::error!(err = %e, "network runtime failed to start");
            eprintln!(
                "✗ Rill: the network runtime failed to start: {e}. Restart Rill; if it keeps happening, restart the computer."
            );
            return ExitCode::FAILURE;
        }
    };
    let http = {
        let _runtime = hub.handle().enter();
        reqwest_client::ReqwestClient::user_agent(concat!("Rill/", env!("CARGO_PKG_VERSION")))
    };
    let mut app = gpui_platform::application().with_assets(assets::Assets);
    match http {
        Ok(client) => app = app.with_http_client(Arc::new(client)),
        Err(e) => tracing::warn!(err = %e, "HTTP client failed; emotes show as text"),
    }

    let exit_after = options.exit_after;
    let history = options.history;
    let overlay = options.overlay;
    // Simulated chat (not the firehose) marks first-time chatters after a minute, to show them.
    let simulated = options.sim && options.firehose.is_none();
    let stats_path = options.stats.clone();
    app.run(move |cx: &mut App| {
        gpui_component::init(cx);
        Theme::change(ThemeMode::Dark, None, cx);
        style::tune_theme(cx);
        composer::init(cx);
        focus::init(cx);
        panels::init(cx);
        search::init(cx);
        navigate::init(cx);

        // Reopen where the window was left; simulated runs always start the same way.
        let saved = (!options.sim)
            .then(rill_os::window::load)
            .flatten()
            .map(|p| {
                let b = Bounds::new(point(px(p.x), px(p.y)), size(px(p.width), px(p.height)));
                (b, p.maximized)
            })
            .filter(|(b, _)| cx.displays().iter().any(|d| d.bounds().intersects(b)));
        let window_bounds = match saved {
            Some((b, true)) => WindowBounds::Maximized(b),
            Some((b, false)) => WindowBounds::Windowed(b),
            None => WindowBounds::Windowed(Bounds::centered(None, size(px(440.), px(720.)), cx)),
        };
        let window_options = WindowOptions {
            window_bounds: Some(window_bounds),
            titlebar: Some(TitlebarOptions {
                title: Some("Rill".into()),
                ..Default::default()
            }),
            app_id: Some("com.mobasity.rill".into()),
            ..Default::default()
        };
        let mut view_handle = None;
        let opened = cx.open_window(window_options, |window, cx| {
            let view = cx.new(|cx| {
                chat_view::ChatView::new(
                    hub,
                    platforms,
                    events,
                    startup_errors,
                    chat_view::ViewOptions {
                        history,
                        simulated,
                        overlay,
                    },
                    window,
                    cx,
                )
            });
            view_handle = Some(view.clone());
            cx.new(|cx| Root::new(view, window, cx))
        });
        let window = match opened {
            Ok(w) => w,
            Err(e) => {
                tracing::error!(err = %e, "window failed to open");
                eprintln!("✗ GPUI: could not open the window: {e}. Update the graphics driver and try again.");
                cx.quit();
                return;
            }
        };
        if !options.sim {
            let _ = window.update(cx, |_, window, cx| {
                window.on_window_should_close(cx, |window, _| {
                    let (b, maximized) = match window.window_bounds() {
                        WindowBounds::Windowed(b) => (b, false),
                        WindowBounds::Maximized(b) | WindowBounds::Fullscreen(b) => (b, true),
                    };
                    let placement = rill_os::window::Placement {
                        x: f32::from(b.origin.x),
                        y: f32::from(b.origin.y),
                        width: f32::from(b.size.width),
                        height: f32::from(b.size.height),
                        maximized,
                    };
                    if let Err(e) = rill_os::window::save(&placement) {
                        tracing::warn!(err = %e, "window placement not saved");
                    }
                    true
                });
            });
        }
        cx.activate(true);

        if let (Some(after), Some(view)) = (exit_after, view_handle) {
            cx.spawn(async move |cx| {
                cx.background_executor().timer(after).await;
                cx.update(|cx| {
                    let v = view.read(cx);
                    let s = v.stats;
                    let report = format!(
                        "seconds={:.1}\nevents={}\nbatches={}\nrenders={}\nrows={}\nfeed={}\nimages={}\n",
                        v.uptime().as_secs_f64(),
                        s.events,
                        s.batches,
                        s.renders,
                        s.rows,
                        v.session.feed.len(),
                        v.images.read(cx).len(),
                    );
                    tracing::info!(report = %report.replace('\n', " "), "benchmark finished");
                    if let Some(path) = &stats_path
                        && let Err(e) = std::fs::write(path, &report)
                    {
                        tracing::error!(err = %e, path = %path.display(), "stats file not written");
                    }
                    cx.quit();
                });
            })
            .detach();
        }
    });
    ExitCode::SUCCESS
}
