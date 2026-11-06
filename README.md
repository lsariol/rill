# Rill

A fast native desktop chat client for multistreamers: Twitch, YouTube and Kick chat in one
chronological feed, with one composer that sends to every platform or just one.

Rust, [GPUI](https://gpui.rs) and Tokio. Windows first; macOS and Linux later from the same code.

## Run it

On Windows, in PowerShell, from the repository folder:

```powershell
cargo run -p rill-app            # connect to the real platforms
cargo run -p rill-app -- --sim   # simulated chat, no accounts needed
cargo run -p rill-app -- --help  # all options
```

A debug run keeps everything it writes (settings, logs) in the repository's `.dev/` folder. Tokens
go to Windows Credential Manager under `rill-dev`. Release builds use `%LOCALAPPDATA%\Rill` and
`rill`.

## Using it

- **Composer:** Enter sends, Shift+Enter starts a new line. Tab and Shift+Tab switch the
  destination: all platforms (rainbow outline) or one platform (its color); the label above says
  which.
- **Commands:** type `/` for the list. `/ban`, `/timeout`, `/unban` and `/poll` run on a platform's
  API; with "All platforms" selected Rill asks which platform. `/clear` and `/help` are Rill's own.
  Start a message with `//` to send a literal slash.
- **Moderation:** click a message to open the user card (delete, timeouts, ban, unban), showing
  only what that platform allows.
- **Header:** click a platform icon to hide or show its messages (or to connect it), `@` for
  mentions only, `≡` for compact mode. Narrow windows go compact by themselves.
- **Accounts:** connect and disconnect platforms.

## Platform setup

Rill needs an app registration on each platform. Until release builds carry them, put them in
`settings.json` (in `.dev/` for debug runs):

```json
{
  "twitch": { "client_id": "…", "channel": null },
  "youtube": { "client_id": "…", "client_secret": "…", "min_poll_ms": 5000 },
  "kick": { "channel": "somechannel" }
}
```

- **Twitch:** at dev.twitch.tv/console, register an app with **Client Type: Public** and the
  OAuth redirect URL `http://localhost` (unused; sign-in uses a device code). `channel` reads
  another channel instead of your own.
- **YouTube:** in Google Cloud, enable the YouTube Data API v3 and create an OAuth client of type
  **Desktop app**. Chat appears while you're live. Polling uses API quota (about 5 units per poll;
  the default daily quota is 10,000); raise `min_poll_ms` to stretch it.
- **Kick:** read-only, and needs no account. Rill reads chat from Kick's unofficial chat socket, as an
  anonymous viewer would; Kick can change it without notice.

Release builds can carry the IDs instead: set `RILL_TWITCH_CLIENT_ID`, `RILL_YOUTUBE_CLIENT_ID` and
`RILL_YOUTUBE_CLIENT_SECRET` when building.

## Checks and measurements

```powershell
powershell -File scripts/check.ps1   # rustfmt, Clippy, tests, cargo-deny
powershell -File scripts/bench.ps1   # startup, CPU, memory and GPU under simulated chat
powershell -File scripts/soak.ps1 -Minutes 480   # long-stream memory check
```

## Layout

| Crate | What it does |
|---|---|
| `rill-core` | Messages, the feed, slash commands, the session and the adapter hub. No UI or OS code. |
| `rill-sim` | Simulated chat traffic, for prototyping and benchmarks |
| `rill-twitch` | Twitch: device-code sign-in, EventSub chat, Helix actions |
| `rill-youtube` | YouTube: desktop sign-in with PKCE, live chat polling, moderation |
| `rill-kick` | Kick: asks Kick which chat socket to use (Centrifugo or Pusher) and reads chat from it |
| `rill-os` | Data folders, `settings.json`, the OS credential store |
| `rill-app` | The GPUI app; the only crate that depends on GPUI |

## License

MIT
