//! Crash reports and logs, sent to rill.mobasity.com only when the user chooses to.

use std::path::PathBuf;
use std::sync::Arc;

use futures::AsyncReadExt;
use gpui::http_client::{AsyncBody, HttpClient};

/// Where reports go.
const ENDPOINT: &str = "/api/reports/v1";

/// How much of the recent log a report carries: its newest lines.
const LOG_BYTES: usize = 600 * 1024;

/// What a report says happened.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Kind {
    /// Sent after a problem recorded in `crash.txt`.
    Crash,
    /// Sent from Settings.
    Logs,
}

/// Where sending a report stands.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) enum Sending {
    #[default]
    Idle,
    Busy,
    /// Sent; the report's ID, to quote in an email.
    Sent(String),
    Failed(String),
}

fn path() -> PathBuf {
    rill_os::data_dir().join("crash.txt")
}

/// Records every panic, on any thread, before the usual handling.
pub(crate) fn install_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current()
            .name()
            .unwrap_or("unnamed")
            .to_string();
        let text = format!(
            "Rill {} had a problem at {} on thread \"{thread}\".\n{info}\n\nBacktrace:\n{}",
            env!("CARGO_PKG_VERSION"),
            chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC"),
            std::backtrace::Backtrace::force_capture()
        );
        tracing::error!(thread, panic = %info, "crashed");
        let _ = std::fs::create_dir_all(rill_os::data_dir());
        let _ = std::fs::write(path(), &text);
        default(info);
    }));
}

/// The license notices installed beside Rill, when it's an installed copy.
pub(crate) fn licences() -> Option<PathBuf> {
    let path = std::env::current_exe()
        .ok()?
        .parent()?
        .join("third-party-licenses.html");
    path.exists().then_some(path)
}

/// The problem recorded last time Rill ran, if any.
pub(crate) fn last() -> Option<String> {
    std::fs::read_to_string(path())
        .ok()
        .filter(|t| !t.trim().is_empty())
}

/// Forgets the recorded problem, once it's sent or dismissed.
pub(crate) fn forget() {
    let _ = std::fs::remove_file(path());
}

/// The newest lines of the two newest log files.
fn recent_log() -> String {
    let dir = rill_os::log_dir();
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .map(|entries| {
            entries
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with("rill") && n.ends_with(".log"))
                })
                .collect()
        })
        .unwrap_or_default();
    // Daily files are named by date, so the newest sort last.
    files.sort();
    let mut text = String::new();
    for file in files.iter().rev().take(2).rev() {
        text.push_str(&tail(file, LOG_BYTES));
    }
    if text.len() > LOG_BYTES {
        let mut from = text.len() - LOG_BYTES;
        while !text.is_char_boundary(from) {
            from += 1;
        }
        text = text[from..].to_string();
    }
    text
}

/// The last `bytes` of a file, without reading the rest.
fn tail(file: &std::path::Path, bytes: usize) -> String {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut f) = std::fs::File::open(file) else {
        return String::new();
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let _ = f.seek(SeekFrom::Start(len.saturating_sub(bytes as u64)));
    let mut buf = Vec::new();
    let _ = f.take(bytes as u64).read_to_end(&mut buf);
    String::from_utf8_lossy(&buf).into_owned()
}

/// The report to send, as JSON.
pub(crate) fn report(kind: Kind, crash: Option<&str>) -> String {
    serde_json::json!({
        "kind": match kind {
            Kind::Crash => "crash",
            Kind::Logs => "logs",
        },
        "version": env!("CARGO_PKG_VERSION"),
        "os": format!("{} {}", std::env::consts::OS, std::env::consts::ARCH),
        "crash": crash.unwrap_or_default(),
        "log": recent_log(),
    })
    .to_string()
}

/// Sends a report to the website and returns its ID.
pub(crate) async fn send(client: Arc<dyn HttpClient>, report: String) -> Result<String, String> {
    // RILL_REPORT_URL points a test run at a local copy of the website.
    let url = std::env::var("RILL_REPORT_URL")
        .ok()
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| format!("{}{ENDPOINT}", rill_core::WEBSITE));
    let mut response = client
        .post_json(&url, AsyncBody::from(report))
        .await
        .map_err(|e| {
            format!(
                "Couldn't reach {} ({e}). Check the connection and try again.",
                rill_core::WEBSITE
            )
        })?;
    let status = response.status();
    let mut body = String::new();
    let _ = response.body_mut().read_to_string(&mut body).await;
    let answer: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
    if status.is_success()
        && let Some(id) = answer["id"].as_str()
    {
        return Ok(id.to_string());
    }
    Err(answer["error"]
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| format!("The website answered {status}. Try again later.")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_report_names_its_kind_and_version() {
        let r: serde_json::Value =
            serde_json::from_str(&report(Kind::Crash, Some("panicked at x"))).unwrap();
        assert_eq!(r["kind"], "crash");
        assert_eq!(r["crash"], "panicked at x");
        assert_eq!(r["version"], env!("CARGO_PKG_VERSION"));
        assert!(r["log"].is_string());
    }
}
