# Changelog

## v0.0.0 (unreleased)

Rill's first release, in progress.

### Added
- One chronological chat feed for Twitch, YouTube and Kick. Each message shows its platform's icon, the time, badges, the name in the user's color, and emote images. Mentions of you are highlighted.
- The feed keeps the newest 5,000 lines and only draws what's on screen. Scrolling up pauses it, with a "new messages" button to jump back.
- Header filters: hide or show each platform, show only mentions, and a compact mode that also turns on by itself in narrow windows.
- The composer's outline shows where a message goes: a rainbow for every platform, or the platform's own color, with a text label. Tab and Shift+Tab switch the destination without losing what's typed. Enter sends; Shift+Enter starts a new line.
- Sending to all platforms shows a ✓ or ✗ for each one, with the reason when a platform refuses.
- Slash commands with autocomplete: `/ban`, `/timeout`, `/unban`, `/poll`, `/clear` and `/help`. Platform commands with "All platforms" selected ask which platform to use. `//` sends a literal slash.
- Clicking a message opens a user card with the moderation actions that platform allows: delete, timeouts, ban and unban.
- A poll form (question, 2 to 5 choices, length) for Twitch.
- Accounts: connect Twitch with a device code, YouTube through the browser, and pick a Kick channel to read. Tokens are kept in the system's credential store.
- Twitch: chat, deletions and clears over EventSub, sending and replies, moderation and polls, with automatic token refresh and reconnects.
- YouTube: the live chat of your current stream, sending, deleting, timeouts, bans and unbans, with a warning before the daily API quota runs out.
- Kick: read-only chat for any channel, without an account, over whichever chat socket Kick offers.
- 7TV emotes: the channel's set on Twitch and Kick and 7TV's global set everywhere, animated, at their own sizes, with zero-width emotes drawn over the emote before them. Emotes the streamer adds, removes or renames update live.
- Keyboard use: F6 moves between the composer and the buttons, Tab and Shift+Tab move between buttons, Esc returns to the composer, and Esc then Tab leaves it.
- `--sim` shows simulated chat, with options for rate, bursts, emotes, failures and dropped connections.
