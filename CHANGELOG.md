# Changelog

## v0.0.0 (unreleased)

Rill's first release, in progress.

### Added
- One chronological chat feed for Twitch, YouTube, Kick and Rumble. Each message shows its platform's icon, the time, badges, the name in the user's color, and emote images. Mentions of you are highlighted in red, whichever platform's name they use.
- The feed keeps the newest 5,000 lines and only draws what's on screen. Scrolling up pauses the feed and shows a scrollbar for dragging back through older messages, with a "new messages" button to jump back.
- Header filters: hide or show each platform, show only mentions, and a compact mode that also turns on by itself in narrow windows.
- The composer's outline shows where a message goes: a rainbow for every platform, or the platform's own color, with a text label. Tab and Shift+Tab switch the destination without losing what's typed. Enter sends; Shift+Enter starts a new line.
- Sending to all platforms shows a ✓ or ✗ for each one, with the reason when a platform refuses. A character count appears near the strictest destination's limit, and a message too long for any destination is refused before it's sent anywhere.
- Slash commands with autocomplete: `/ban`, `/timeout`, `/unban`, `/poll`, `/clear` and `/help`. Platform commands with "All platforms" selected ask which platform to use. `//` sends a literal slash.
- `/ad [length]` starts an ad break on Twitch, Kick and YouTube; every ad break, including automatic ones, gets a line when it starts (with its length) and when it ends.
- Twitch channel-point redemptions, custom and built-in, show as event lines with what the viewer typed.
- Kick subs, gifted subs, hosts and redemptions show as event lines, read from Kick's chat socket without an account.
- `/set title` and `/set category` (also `/title` and `/category`): change the stream title on Twitch, Kick and YouTube and the category on Twitch and Kick at once, with one summary line.
- `/purge`, Twitch chat modes (`/slow`, `/followers`, `/subscribers`, `/emoteonly`, `/uniquechat`), `/announce`, `/shoutout` and `/search`. A command that only one connected platform can run goes there without asking. A Twitch sign-in from before these commands says to reconnect, instead of looking expired.
- `/nuke`, `/meganuke` and `/unnuke`: time out or ban everyone who recently said a phrase, on every platform or one, after a confirmation that shows the count per platform; undo the last one. Moderators, VIPs, the broadcaster and you are left out, and Twitch rate limits are waited out instead of failing.
- Replies: from a user card or with R on a picked message, on Twitch and Kick; the composer quotes the message, and replies in chat show what they answer.
- The user card lists the user's last five messages this session, deleted ones struck through.
- Clicking a message opens a user card with the moderation actions that platform allows: delete, timeouts, ban and unban.
- A poll form (question, 2 to 5 choices, length) for Twitch.
- Accounts: connect Twitch with a device code, YouTube through the browser, and pick a Kick channel to read. Tokens are kept in the system's credential store.
- Twitch: chat, deletions, clears, and subs, gifts, raids and announcements (as event lines) over EventSub, sending and replies, moderation and polls, with automatic token refresh and reconnects.
- YouTube: the live chat of your current stream, read over YouTube's streaming chat so it uses a few quota units an hour instead of thousands (polling takes over if the stream isn't available), plus sending, deleting, timeouts, bans and unbans, with a warning before the daily API quota runs out.
- Kick: chat for any channel, without an account, over whichever chat socket Kick offers. Signing in through Kick's official API adds sending, replies, deleting messages, timeouts, bans and unbans; reading your own channel starts by itself after sign-in.
- Connecting an account opens the sign-in page in the browser straight away; Accounts keeps Open page and Copy link for using another browser.
- Settings, with a switch for each platform: a platform turned off disconnects and disappears from the chat, the header and Accounts, keeps its sign-in, and stays off after a restart.
- Rumble: read-only live chat, Rants, follows and subscriptions through Rumble's Live Stream API, connected by pasting the private link from Rumble's account settings; the link is kept in the system's credential store.
- 7TV emotes: the channel's set on Twitch and Kick and 7TV's global set everywhere, animated, at their own sizes, with zero-width emotes drawn over the emote before them. Emotes the streamer adds, removes or renames update live. BetterTTV and FrankerFaceZ emotes too: the channel's sets on Twitch and their global sets everywhere.
- Emote suggestions: typing `:` and the start of a name in the composer lists the emotes you can use on the platforms the message goes to (your Twitch emotes including subscriptions, Kick's, and 7TV, BetterTTV and FrankerFaceZ), and Tab or Enter fills one in. Kick emote names in sent messages show as emotes.
- Ctrl+F (Cmd+F on macOS) searches the feed: only messages whose text or author matches stay visible, with a count. Esc closes the search.
- Keyboard use: F6 moves between the composer and the messages; arrow keys pick a message and Enter opens its user card; Tab and Shift+Tab move between buttons; Esc closes a card and returns to the composer, and Esc then Tab leaves the composer.
- Rill reopens at the size and place it was closed (kept in `window.json` beside `settings.json`), unless that spot is no longer on a screen.
- `--sim` shows simulated chat, with options for rate, bursts, emotes, failures and dropped connections.
