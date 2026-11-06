# Rill

A fast native desktop chat client for multistreamers: Twitch, YouTube, Kick and Rumble chat in one
chronological feed, with one composer that sends to every platform or just one.

Rust, [GPUI](https://gpui.rs) and Tokio. Windows first; macOS and Linux later from the same code.

## Run it

On Windows, in PowerShell, from the repository folder:

```powershell
cargo run -p rill-app            # connect to the real platforms
cargo run -p rill-app -- --sim   # simulated chat, no accounts needed
cargo run -p rill-app -- --help  # all options
```

A debug run keeps everything it writes (settings, window placement, logs) in the repository's
`.dev/` folder, and tokens go to Windows Credential Manager under `rill-dev`. Release builds use
`%LOCALAPPDATA%\Rill` and `rill`. Set `RILL_DATA_DIR` to keep settings and logs in another
folder; the benchmark scripts use `.dev\bench\data`.

## Using it

- **Composer:** Enter sends, Shift+Enter starts a new line. Tab and Shift+Tab switch the
  destination: all platforms (rainbow outline) or one platform (its color); the label above says
  which. Near the shortest limit among the destinations (200 characters on YouTube, 500 on Twitch
  and Kick) a count appears, and a message that's too long isn't sent anywhere.
- **Commands:** type `/` for the list, or `/help`. Moderation: `/ban`, `/timeout`, `/unban`, and
  `/purge <user>` to clear someone's messages with a one-second timeout. Twitch chat modes:
  `/slow [3s-2m|off]`, `/followers [10m|off]`, `/subscribers`, `/emoteonly` and `/uniquechat`
  (`on` or `off`). Twitch also has `/announce <message>`, `/shoutout <user>` and `/poll`. With
  "All platforms" selected, Rill asks which platform when more than one can do it.
  `/search <text>`, `/clear` and `/help` are Rill's own. Start a message with `//` to send a
  literal slash.
- **Nukes:** for bot raids and spam waves. `/nuke <phrase>` times out everyone who said the phrase
  in the last 5 minutes for 24 hours; `/nuke haha 48h` sets the timeout, and `/nuke haha 48h 15m`
  also looks back 15 minutes. `/meganuke <phrase> [lookback]` bans instead. Put a platform first
  (`/nuke twitch haha`) to act on one platform only, and quotes around a phrase that starts with a
  platform name or ends with a duration. The broadcaster, moderators, VIPs and you are never
  included. Rill shows how many users each platform would hit and waits for Enter, then reports
  one summary line. `/unnuke` lifts the last nuke's bans and timeouts.
- **Replies:** Reply on a user card (or R on a message picked with the keyboard) answers that
  message on its platform (Twitch and Kick); the composer quotes it until you send or press Esc.
  Replies in chat show the message they answer.
- **Moderation:** click a message to open the user card: delete, timeouts, ban and unban, showing
  only what that platform allows, and the user's last five messages this session.
- **Emotes:** platform emotes, plus 7TV, BetterTTV and FrankerFaceZ. The channel's 7TV set loads on
  Twitch and Kick (changes show up without a restart), its BetterTTV and FrankerFaceZ sets on
  Twitch, and the three services' global emotes everywhere.
- **Header:** click a platform icon to hide or show its messages (or to connect it), `@` for
  mentions only, `≡` for compact mode. Narrow windows go compact by themselves.
- **Accounts:** connect and disconnect platforms.
- **Search:** Ctrl+F (Cmd+F on macOS) shows only the messages whose text or author name contains
  what you type, with a count of matches. Esc closes it.
- **Scrolling:** scroll up (or press PgUp) to pause the feed; a scrollbar appears for dragging
  back through older messages, and a button jumps back to live chat.
- **Keyboard:** F6 moves between the composer and the messages. In the messages, Up and Down (or
  Home and End) pick one, and Enter or Space opens its user card. Tab and Shift+Tab move between
  buttons, and Enter or Space presses one. In the composer, press Esc (with nothing to close),
  then Tab, to leave it. Esc anywhere else closes an open card and returns to the composer.

## Platform setup

Rill needs an app registration on each platform. Until release builds carry them, put them in
`settings.json` (in `.dev/` for debug runs):

```json
{
  "twitch": { "client_id": "…", "channel": null },
  "youtube": { "client_id": "…", "client_secret": "…", "min_poll_ms": 5000 },
  "kick": { "channel": "somechannel", "client_id": "…", "client_secret": "…" }
}
```

- **Twitch:** at dev.twitch.tv/console, register an app with **Client Type: Public** and the
  OAuth redirect URL `http://localhost` (unused; sign-in uses a device code). `channel` reads
  another channel instead of your own.
- **YouTube:** in Google Cloud, enable the YouTube Data API v3 and create an OAuth client of type
  **Desktop app**. Chat appears while you're live. Polling uses API quota (about 5 units per poll;
  the default daily quota is 10,000); raise `min_poll_ms` to stretch it.
- **Kick:** reading needs no account: Rill reads chat from Kick's unofficial chat socket, as an
  anonymous viewer would, and Kick can change it without notice. Sending and moderating use Kick's
  official API: at kick.com, under Settings → Developer, create an app with the redirect URL
  `http://localhost:47831/callback` and the scopes `user:read`, `channel:read`, `chat:write`,
  `moderation:ban` and `moderation:chat_message:manage`. Kick's sign-in needs the app's client
  secret, so a development build takes `client_secret` from `settings.json`; a release build instead
  sets `auth_helper` to a small token-exchange service that holds the secret. Sign-in listens on
  port 47831 while it waits for Kick. Kick timeouts last at most 7 days.
- **Rumble:** read-only. In Accounts, paste the private link from rumble.com/account/livestream-api
  (it starts `https://rumble.com/-livestream-api/get-data?key=`); Rill keeps it in the system's
  credential store, not in `settings.json`. Rumble's Live Stream API is polled every 10 seconds,
  so its chat arrives up to 10 seconds late, and Rants show as events with their amount.

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
| `rill-kick` | Kick: reads chat from Kick's chat socket (Centrifugo or Pusher); signs in, sends and moderates through the official API |
| `rill-rumble` | Rumble: polls the Live Stream API for live chat and Rants (read-only) |
| `rill-emotes` | 7TV, BetterTTV and FrankerFaceZ emotes; follows 7TV set changes over its event socket |
| `rill-os` | Data folders, `settings.json`, the OS credential store |
| `rill-app` | The GPUI app; the only crate that depends on GPUI |

## License

MIT
