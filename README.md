# Rill

A fast native desktop chat client for multistreamers: Twitch, YouTube, Kick and Rumble chat in one
chronological feed, with one composer that sends to every platform or just one.

Rill is written in Rust with [GPUI](https://gpui.rs) and Tokio. It runs on Windows today; Linux is
next, from the same code.

## What it does

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
  "All platforms" selected, Rill asks which platform when more than one can do it: click one, or
  pick it with Tab or the arrow keys and press Enter (Esc cancels).
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
- **Moderation:** click a name to open the user card, which floats over the chat beside the
  message while chat keeps moving: delete, timeouts, ban and unban, showing only what that
  platform allows, and every message from that user still in the chat. Actions leave the card
  open and the chat where it is; clicking anywhere else closes it.
- **Badges:** hovering a badge says what it is, with how long a subscriber has subscribed.
  Channels' own badges are used: custom subscriber badges on Twitch and Kick, and the moderator
  and VIP badges a Twitch channel sets on FrankerFaceZ.
- **Copying:** drag across messages to select them and press Ctrl+C. Each message copies as a
  line with its time (`15:44 name: message`), and emotes copy as their names.
- **Links:** links open in the default browser. Hovering one shows the page's title, description
  and picture; **Link previews** in Settings turns that off (`link_previews`), and hovering then
  shows only the address. Rill fetches a preview only for the link you're hovering, and never for
  addresses on your computer or local network.
- **Emotes:** hovering an emote in chat shows it bigger, with its name and where it comes from
  (such as "7TV channel emote" or "Twitch emote from another channel").
  Platform emotes, plus 7TV, BetterTTV and FrankerFaceZ. The channel's 7TV set loads on
  Twitch and Kick (changes show up without a restart), its BetterTTV and FrankerFaceZ sets on
  Twitch, and the three services' global emotes everywhere. In the composer, type `:` and the
  start of a name (`:sm`) for a list of the emotes you can use where the message is going, with
  the platforms each works on; Tab or Enter picks one, Up and Down move, Esc closes the list. A
  `:` alone lists them all, the channel's own first; scroll with the mouse wheel, PgUp and PgDn. On
  Twitch this includes your subscription emotes from other channels. Without the `:`, Tab after
  the start of a word puts in the emotes and names that start with it one at a time,
  alphabetically (`s`, then `sadge`, `smash`, `smile`), and Shift+Tab goes back; with no match,
  Tab switches the destination as usual.
- **Mentions:** type `@` and the start of a name for a list of the people who have chatted since
  Rill started, alphabetically: on the destination's platform, or on every platform for All, with
  the platforms each was seen on. Tab or Enter fills in `@name`. Tab completion on a plain word
  includes these names too, so `moba` then Tab gives `mobasity`.
- **Header:** click a platform icon to hide or show its messages, `@` for mentions only, `≡`
  for compact mode. Narrow windows go compact by themselves. The dot on each icon shows its
  connection: green when it's working, a green ring when it's signed in and waiting (YouTube
  before you go live), pulsing yellow while connecting, yellow when it works but needs something
  from you (an older sign-in, or Kick read without signing in), red when it isn't working, and
  grey when it isn't signed in. Hovering the icon says why; clicking a yellow, red or grey one
  opens Accounts.
- **Accounts:** connect and disconnect platforms. Connect opens the platform's sign-in page in the
  default browser; Copy link is there for signing in with another browser.
- **Settings:** turn off the platforms you don't use. A platform that's off isn't connected and
  doesn't appear in the chat, the header or Accounts; its sign-in is kept for when you turn it
  back on. The choice is saved in `settings.json` as `disabled_platforms`. **Show deleted
  messages** keeps the text of deleted messages, faded and struck through, instead of "message
  deleted" (`show_deleted_messages`). **Message history** sets how many chat lines Rill keeps
  for scrolling back, searching and user cards (`message_history`):

  | Lines | Rill's memory with it full | |
  |---|---|---|
  | 1,000 | about 100 MB | lightest |
  | 5,000 | about 110 MB | recommended, the default |
  | 20,000 | about 140 MB | long streams |
  | 50,000 | about 190 MB | big events |
  | 100,000 | about 280 MB | most |

  Each 1,000 lines takes about 2 MB (measured with simulated chat; long messages take a little
  more). Only what's on screen is drawn, so more history doesn't slow chat down, though a search
  goes through all of it. Settings also says how long each choice lasts at the current pace of
  chat. `--history <n>` overrides the setting for one run.
- **Search:** Ctrl+F (Cmd+F on macOS) shows only the messages whose text or author name contains
  what you type, with a count of matches. Esc closes it.
- **Scrolling:** scroll up (or press PgUp) to pause the feed, or drag the scrollbar on the right,
  which widens under the pointer; a button jumps back to live chat.
- **Highlights:** messages that mention you, first-time chatters, and events (subscriptions,
  gifted subs, raids, Bits, Rants and Super Chats, redemptions, follows, announcements and ad
  breaks) each get a colored bar and tint. Settings → Highlights turns each on or off and picks
  its color from the swatches or any hex code (`#ff00aa`). Thin marks along the scrollbar show
  where highlighted lines are, so a sub that scrolled away while you were gone is easy to find.
  First-time chatters are off until you turn them on: the platforms don't tell apps when
  someone chats for the first time, so Rill keeps a list of who has chatted in your channels
  (`known-chatters.txt` beside `settings.json`, about 80 bytes of memory per person) and starts
  marking first-timers once it has watched a platform's chat for a week. Turned off, the list
  leaves memory, and Settings offers to delete the saved file.
- **Text size:** Settings → Text size makes chat, emotes, badges and the rest of the window
  90 % to 150 % of normal (`text_size`); Ctrl+= and Ctrl+- change it from anywhere, and Ctrl+0
  puts it back.
- **OBS overlay:** Settings → OBS overlay shows your chat on stream. Turned on, Rill serves a
  page at `http://127.0.0.1:47832/` (Copy link is beside it); in OBS, add a Browser source with
  that address, and set its Width and Height to the space chat should fill, such as 400 by 900.
  To resize it later, change Width and Height in the source's properties: dragging its corners
  only stretches the picture, so no more lines fit. Lines sit at the bottom and move up, with
  emotes, badges and platform icons. Settings picks the text size, whether lines fade after a
  while, badges and icons, a dark backing, and hiding commands (`!uptime`) and well-known bots.
  The overlay's highlights are its own, separate from Rill's and all off at first, so the stream
  shows plain chat until you choose otherwise. Deleted messages leave the overlay too. Only this
  computer can open the page, and it runs only while the setting is on and Rill is open; the
  source's Custom CSS box in OBS changes the look further (`.line`, `.name`, `.text`, `.badge`,
  `.icon`, `.emote`, and `.hl` for highlighted lines). The port is `overlay.port` in
  `settings.json`, and `--overlay <port>` turns it on for one run.
- **Problems and logs:** if Rill hits a problem, it records it, and the next start offers to send
  a report to the Rill team: what went wrong and the end of Rill's recent log, never chat
  messages or passwords. Settings → About can send the log at any time too. Either way, Rill shows
  the report's ID to mention when emailing rill@mobasity.com. Nothing is sent unless you choose
  to. About also has the version, links to the website, and the open-source license notices of
  everything Rill is built with.
- **Keyboard:** F6 moves between the composer and the messages. In the messages, Up and Down (or
  Home and End) pick one, and Enter or Space opens its user card. Tab and Shift+Tab move between
  buttons, and Enter or Space presses one. In the composer, press Esc (with nothing to close),
  then Tab, to leave it. Esc anywhere else closes an open card and returns to the composer.

## Requirements

- To use Rill: Windows 10 or 11.
- To build it: [Rust](https://rustup.rs) (stable) with the MSVC toolchain, which needs Visual
  Studio's C++ build tools.
- To build the installer: the .NET 8 SDK, Velopack's packaging tool (`dotnet tool install -g
  vpk`) and cargo-about (`cargo install --locked cargo-about --features cli`), which writes the
  license notices shipped with Rill.
- For the full checks: cargo-deny (`cargo install --locked cargo-deny`).

## Getting started

Download the installer from [rill.mobasity.com](https://rill.mobasity.com) and run it. It installs
to `%LocalAppData%\Rill` without asking for administrator rights, and adds Rill to the Start menu,
the desktop and Windows' list of installed apps. Then open **Accounts** in Rill and connect your
platforms.

YouTube is limited for now: until Google finishes reviewing Rill, only Google accounts on Rill's
test list can connect it, and its sign-in lasts 7 days. To be added, email
[rill@mobasity.com](mailto:rill@mobasity.com) with the Google account you stream from.

To run it from source instead, in PowerShell from the repository folder:

```powershell
cargo run -p rill-app            # connect to the real platforms
cargo run -p rill-app -- --sim   # simulated chat, no accounts needed
cargo run -p rill-app -- --help  # all options
```

## Configuration

Settings are chosen in Rill's Settings panel and saved in `settings.json`. An installed Rill keeps
it, with its logs, in `%AppData%\Rill`, and sign-ins in Windows Credential Manager under `rill`, so
both survive an uninstall. A debug run keeps everything it writes (settings, window placement,
logs) in the repository's `.dev/` folder instead, with sign-ins under `rill-dev`. Set
`RILL_DATA_DIR` to keep settings and logs in another folder; the benchmark scripts use
`.dev\bench\data`.

### Updates

Rill installs and updates itself with [Velopack](https://velopack.io). An installed Rill checks
`https://rill.mobasity.com/updates` when it starts and every six hours, downloads a new version
in the background, and installs it the next time it starts; **Update** in the header installs it
right away. Settings → Updates turns the checks off (`auto_update`). A copy run from source
doesn't update.

### Platform setup

Twitch, YouTube and Kick each need an app registration. Until release builds carry them, put them
in `settings.json` (in `.dev/` for debug runs):

```json
{
  "twitch": { "client_id": "…", "channel": null },
  "youtube": { "client_id": "…", "client_secret": "…" },
  "kick": { "channel": "somechannel", "client_id": "…", "client_secret": "…" }
}
```

- **Twitch:** at dev.twitch.tv/console, register an app with **Client Type: Public** and the
  OAuth redirect URL `http://localhost` (unused; sign-in uses a device code). `channel` reads
  another channel instead of your own.
- **YouTube:** in Google Cloud, enable the YouTube Data API v3 and create an OAuth client of type
  **Desktop app**. Chat appears while you're live. Rill reads it only over YouTube's streaming chat
  (gRPC), which costs 5 quota units each time it connects rather than every few seconds; it never
  falls back to polling, which would use up the daily quota (10,000 units by default) far faster.
  If the stream fails, Rill retries less and less often and says so. Sending, deleting and bans
  use the Data API (50 units each).
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

Release builds can carry the IDs instead: set `RILL_TWITCH_CLIENT_ID`, `RILL_YOUTUBE_CLIENT_ID`,
`RILL_YOUTUBE_CLIENT_SECRET`, `RILL_KICK_CLIENT_ID` and `RILL_KICK_AUTH_HELPER` (the
token-exchange service's address) when building.

## Development

### Checks and measurements

```powershell
powershell -File scripts/check.ps1   # rustfmt, Clippy, tests, cargo-deny
powershell -File scripts/bench.ps1   # startup, CPU, memory and GPU under simulated chat
powershell -File scripts/soak.ps1 -Minutes 480   # long-stream memory check
```

For a load test with real chat, `--firehose <url>` reads a public Twitch chat firehose (in
catquery's `jsonBasic` format, about 1,400 messages a second) and spreads it across the four
platforms; `scripts/soak.ps1 -Firehose <url>` does the same for a long run. It's read-only, and
nothing it receives is saved. Badge images, channels' own subscriber badges included, come from
ivr.fi's public copy of Twitch's badge lists, since the firehose has no Twitch sign-in; each
channel's set is looked up the first time it's seen, two a second at most. The firehose allows
one connection per address, so to run several tests at once, start the relay, which keeps one
upstream connection and serves every test:

```powershell
cargo run --release -p rill-sim --bin firehose-relay   # then each test uses --firehose ws://127.0.0.1:9300
```

### Installer

With the .NET 8 SDK and `vpk` installed (see Requirements), in PowerShell from the repository
folder:

```powershell
powershell -File scripts/package.ps1 -Version 0.1.0
```

It writes `Rill-win-Setup.exe`, a portable zip, and the update feed (`releases.win.json` and
the packages) to `.dev\releases`. Packaging several versions into the same folder makes a feed
that updates from one to the next. To try an update without a server, point an installed Rill
at the folder: `$env:RILL_UPDATE_SOURCE = "<path to .dev\releases>"` before starting it. The
package holds `rill.exe`, with its icon and version details built in, and
`third-party-licenses.html`, generated by cargo-about from `about.toml` and
`scripts/licenses.hbs`; the installer shows `crates/rill-app/assets/app/splash.png` while it
works. To send problem reports to a local copy of the website, set `RILL_REPORT_URL` (such as
`http://127.0.0.1:3190/api/reports/v1`) before starting Rill.

### Releases

Each version's GitHub Release gets its files from `.github/workflows/release.yml`, which runs
when the Release is published. On Windows, it checks that the tag matches the version in
`Cargo.toml`, runs `scripts/check.ps1`, builds and packages with `scripts/package.ps1`, records
build provenance, and adds the installer, the portable zip and the update files to the Release.
The app IDs built in come from the repository's Actions variables `RILL_TWITCH_CLIENT_ID`,
`RILL_YOUTUBE_CLIENT_ID`, `RILL_YOUTUBE_CLIENT_SECRET` and `RILL_KICK_CLIENT_ID`. Installed copies
find these files through `https://rill.mobasity.com/updates`, which points at them.

### Layout

| Crate | What it does |
|---|---|
| `rill-core` | Messages, the feed, slash commands, the session and the adapter hub. No UI or OS code. |
| `rill-sim` | Simulated chat traffic, and a firehose of real chat, for prototyping and load tests |
| `rill-twitch` | Twitch: device-code sign-in, EventSub chat, Helix actions |
| `rill-youtube` | YouTube: desktop sign-in with PKCE, live chat over the gRPC stream, moderation |
| `rill-kick` | Kick: reads chat from Kick's chat socket (Centrifugo or Pusher); signs in, sends and moderates through the official API |
| `rill-rumble` | Rumble: polls the Live Stream API for live chat and Rants (read-only) |
| `rill-emotes` | 7TV, BetterTTV and FrankerFaceZ emotes; follows 7TV set changes over its event socket |
| `rill-os` | Data folders, `settings.json`, the OS credential store |
| `rill-overlay` | The OBS overlay: a small server on 127.0.0.1 and the page it serves |
| `rill-app` | The GPUI app; the only crate that depends on GPUI |

## License

Rill is released under the MIT license. See [LICENSE](LICENSE).
