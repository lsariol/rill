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
  (`on` or `off`). Twitch also has `/announce <message>`, `/shoutout <user>` and `/poll`.
  `/set title <text>` (or `/title`) changes the stream title on Twitch, Kick and YouTube, and
  `/set category <name>` (or `/category`) the category on Twitch and Kick, picking the closest
  match; both go to every platform that can, or only the destination when one is selected.
  `/ad [length]` starts an ad break the same way (60 s by default; Twitch takes 30 s to 3 min,
  Kick 7 to 90 s, and YouTube only on monetized channels). With
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
- **Ads:** whenever an ad break starts, whether from `/ad`, from the platform's dashboard or
  automatically, a line says so with its length, and another when it ends. Twitch reports every
  break; Kick's are checked every 30 seconds; YouTube only reports the ones started from Rill.
- **Channel points:** Twitch and Kick redemptions show as event lines, such as "Foo redeemed
  Hydrate". Kick subs, gifted subs and hosts do too, read from Kick's chat socket.
- **Replies:** Reply on a user card (or R on a message picked with the keyboard) answers that
  message on its platform (Twitch and Kick); the composer quotes it until you send or press Esc.
  Replies in chat show the message they answer.
- **Moderation:** click a message to open the user card: delete, timeouts, ban and unban, showing
  only what that platform allows, and the user's last five messages this session.
- **Emotes:** platform emotes, plus 7TV, BetterTTV and FrankerFaceZ. The channel's 7TV set loads on
  Twitch and Kick (changes show up without a restart), its BetterTTV and FrankerFaceZ sets on
  Twitch, and the three services' global emotes everywhere. In the composer, type `:` and the
  start of a name (`:sm`) for a list of the emotes you can use where the message is going, with
  the platforms each works on; Tab or Enter picks one, Up and Down move, Esc closes the list. On
  Twitch this includes your subscription emotes from other channels.
- **Header:** click a platform icon to hide or show its messages (or to connect it), `@` for
  mentions only, `≡` for compact mode. Narrow windows go compact by themselves.
- **Accounts:** connect and disconnect platforms. Connect opens the platform's sign-in page in the
  default browser; Copy link is there for signing in with another browser.
- **Search:** Ctrl+F (Cmd+F on macOS) shows only the messages whose text or author name contains
  what you type, with a count of matches. Esc closes it.
- **Scrolling:** scroll up (or press PgUp) to pause the feed; a scrollbar appears for dragging
  back through older messages, and a button jumps back to live chat.
- **Keyboard:** F6 moves between the composer and the messages. In the messages, Up and Down (or
  Home and End) pick one, and Enter or Space opens its user card. Tab and Shift+Tab move between
  buttons, and Enter or Space presses one. In the composer, press Esc (with nothing to close),
  then Tab, to leave it. Esc anywhere else closes an open card and returns to the composer.

## Platform setup

Twitch, YouTube and Kick each need an app registration. Until release builds carry them, put them
in `settings.json` (in `.dev/` for debug runs):

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
  **Desktop app**. Chat appears while you're live. Rill reads it over YouTube's streaming chat
  (gRPC), which costs 5 quota units each time it connects rather than every few seconds; if the
  stream isn't available it polls instead (5 units a poll, the default daily quota being 10,000),
  and `min_poll_ms` stretches that. Sending, deleting and bans use the Data API either way.
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
  so its chat arrives up to 10 seconds late. Rants, new followers and new subscribers show as
  event lines.

Release builds can carry the IDs instead: set `RILL_TWITCH_CLIENT_ID`, `RILL_YOUTUBE_CLIENT_ID` and
`RILL_YOUTUBE_CLIENT_SECRET` when building.

## Checks and measurements

```powershell
powershell -File scripts/check.ps1   # rustfmt, Clippy, tests, cargo-deny
powershell -File scripts/bench.ps1   # startup, CPU, memory and GPU under simulated chat
powershell -File scripts/soak.ps1 -Minutes 480   # long-stream memory check
```

For a load test with real chat, `--firehose <url>` reads a public Twitch chat firehose (in
catquery's `jsonBasic` format, about 1,400 messages a second) and spreads it across the four
platforms; `scripts/soak.ps1 -Firehose <url>` does the same for a long run. It's read-only, and
nothing it receives is saved. The firehose allows one connection per address, so to run several
tests at once, start the relay, which keeps one upstream connection and serves every test:

```powershell
cargo run --release -p rill-sim --bin firehose-relay   # then each test uses --firehose ws://127.0.0.1:9300
```

## Layout

| Crate | What it does |
|---|---|
| `rill-core` | Messages, the feed, slash commands, the session and the adapter hub. No UI or OS code. |
| `rill-sim` | Simulated chat traffic, and a firehose of real chat, for prototyping and load tests |
| `rill-twitch` | Twitch: device-code sign-in, EventSub chat, Helix actions |
| `rill-youtube` | YouTube: desktop sign-in with PKCE, live chat over the gRPC stream (or polling), moderation |
| `rill-kick` | Kick: reads chat from Kick's chat socket (Centrifugo or Pusher); signs in, sends and moderates through the official API |
| `rill-rumble` | Rumble: polls the Live Stream API for live chat and Rants (read-only) |
| `rill-emotes` | 7TV, BetterTTV and FrankerFaceZ emotes; follows 7TV set changes over its event socket |
| `rill-os` | Data folders, `settings.json`, the OS credential store |
| `rill-app` | The GPUI app; the only crate that depends on GPUI |

## License

MIT
