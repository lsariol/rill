//! Rill: one chronological chat feed for Twitch, YouTube and Kick.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod assets;
mod chat_view;
mod composer;
mod image_cache;
mod panels;
mod row;
mod style;

use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use gpui::{App, AppContext, Bounds, TitlebarOptions, WindowBounds, WindowOptions, px, size};
use gpui_component::{Root, Theme, ThemeMode};
use rill_core::{Adapter, Hub, Platform};
use rill_sim::{Profile, SimAdapter};

const USAGE: &str = "\
Usage: rill [options]

Options:
  --rate <n>          Simulated messages per second, per platform (default 4)
  --bursts            Ten times the rate for 3 s every 20 s
  --emotes            Make most words emotes
  --flaky             Drop and restore connections now and then
  --fail <0-1>        Share of sends and actions that fail (default 0.05)
  --exit-after <s>    Quit after this many seconds (for benchmarks)
  --stats <file>      Write counters to this file on quit (with --exit-after)
  -h, --help          Show this help

Until accounts can be connected, Rill shows simulated chat.";

/// Command-line options.
#[derive(Debug, Default)]
struct Options {
    profile: Profile,
    exit_after: Option<Duration>,
    stats: Option<std::path::PathBuf>,
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
            "--rate" => o.profile.rate = number(&value("--rate")?, "--rate")?,
            "--fail" => o.profile.fail_rate = number(&value("--fail")?, "--fail")?.clamp(0., 1.),
            "--bursts" => o.profile.bursts = true,
            "--emotes" => o.profile.emote_heavy = true,
            "--flaky" => o.profile.flaky = true,
            "--exit-after" => {
                o.exit_after = Some(Duration::from_secs_f64(number(
                    &value("--exit-after")?,
                    "--exit-after",
                )?))
            }
            "--stats" => o.stats = Some(value("--stats")?.into()),
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
    let level = if cfg!(debug_assertions) {
        "debug"
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

fn main() -> ExitCode {
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
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "Rill starting");

    let adapters: Vec<Box<dyn Adapter>> = Platform::ALL
        .into_iter()
        .map(|p| Box::new(SimAdapter::new(p, options.profile.clone())) as Box<dyn Adapter>)
        .collect();
    let (hub, events) = match Hub::start(adapters) {
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
    let stats_path = options.stats.clone();
    app.run(move |cx: &mut App| {
        gpui_component::init(cx);
        Theme::change(ThemeMode::Dark, None, cx);
        composer::init(cx);

        let bounds = Bounds::centered(None, size(px(440.), px(720.)), cx);
        let window_options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some("Rill".into()),
                ..Default::default()
            }),
            app_id: Some("com.mobasity.rill".into()),
            ..Default::default()
        };
        let mut view_handle = None;
        let opened = cx.open_window(window_options, |window, cx| {
            let view = cx.new(|cx| chat_view::ChatView::new(hub, events, window, cx));
            view_handle = Some(view.clone());
            cx.new(|cx| Root::new(view, window, cx))
        });
        if let Err(e) = opened {
            tracing::error!(err = %e, "window failed to open");
            eprintln!("✗ GPUI: could not open the window: {e}. Update the graphics driver and try again.");
            cx.quit();
            return;
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
