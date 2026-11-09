# Changelog

## v0.0.0 (unreleased)

Rill's first release, in progress.

### Added
- One chronological chat feed for Twitch, YouTube, Kick and Rumble. Each message shows its platform's icon, the time, badges, the name in the user's color, and emote images. Mentions of you are highlighted in red, whichever platform's name they use.
- The feed keeps the newest 5,000 lines by default (Settings offers 1,000 to 100,000, with the memory each takes and how long it lasts at the current pace of chat) and only draws what's on screen. Scrolling up pauses the feed, with a "new messages" button to jump back, and a scrollbar that's always there (wider under the pointer) drags through older messages.
- Header filters: hide or show each platform, show only mentions, and a compact mode that also turns on by itself in narrow windows.
- Connection dots on the header's platform icons: green working, a green ring while YouTube waits for you to go live, pulsing yellow while connecting, yellow when a platform works but needs something (a Twitch sign-in from an older Rill, Kick read without signing in), red when it isn't working and grey when it isn't signed in. The tooltip gives the reasons, and clicking a dot that needs fixing opens Accounts.
- The composer's outline shows where a message goes: a rainbow for every platform, or the platform's own color, with a text label. Tab and Shift+Tab switch the destination without losing what's typed. Enter sends; Shift+Enter starts a new line.
- Sending to all platforms shows a ✓ or ✗ for each one at the end of the line, with the reason when a platform refuses. Your own messages start at the same place as everyone else's. A character count appears near the strictest destination's limit, and a message too long for any destination is refused before it's sent anywhere.
- Slash commands with autocomplete: `/ban`, `/timeout`, `/unban`, `/poll`, `/clear` and `/help`. Platform commands with "All platforms" selected ask which platform to use; the question takes the keyboard, so Tab or the arrow keys pick, Enter runs and Esc cancels. `//` sends a literal slash.
- `/ad [length]` starts an ad break on Twitch, Kick and YouTube; every ad break, including automatic ones, gets a line when it starts (with its length) and when it ends.
- Twitch channel-point redemptions, custom and built-in, show as event lines with what the viewer typed.
- Kick subs, gifted subs, hosts and redemptions show as event lines, read from Kick's chat socket without an account.
- `/set title` and `/set category` (also `/title` and `/category`): change the stream title on Twitch, Kick and YouTube and the category on Twitch and Kick at once, with one summary line.
- `/purge`, Twitch chat modes (`/slow`, `/followers`, `/subscribers`, `/emoteonly`, `/uniquechat`), `/announce`, `/shoutout` and `/search`. A command that only one connected platform can run goes there without asking. A Twitch sign-in from before these commands says to reconnect, instead of looking expired.
- `/nuke`, `/meganuke` and `/unnuke`: time out or ban everyone who recently said a phrase, on every platform or one, after a confirmation that shows the count per platform; undo the last one. Moderators, VIPs, the broadcaster and you are left out, and Twitch rate limits are waited out instead of failing.
- Replies: from a user card or with R on a picked message, on Twitch and Kick; the composer quotes the message, and replies in chat show what they answer.
- Clicking a name opens a user card with the moderation actions that platform allows: delete, timeouts, ban and unban. It floats over the chat beside the message, which stays marked while chat keeps moving, and lists everything that user said that's still in the chat. Its actions leave the card open and the chat where it is, and a click anywhere else closes it.
- Chat text can be selected across messages and copied, each line with its time, and emotes as their names. The time is the first column of every row.
- Hovering an emote in chat shows it bigger, with its name, any zero-width emotes on it, and where it comes from: the service (Twitch, Kick, 7TV, BetterTTV or FrankerFaceZ) and, where known, whether it's the channel's own, global, or from another channel.
- Hovering a badge says at once what it is, including how long a subscriber has subscribed. Twitch channels' custom moderator and VIP badges from FrankerFaceZ are shown, along with Twitch and Kick channels' own subscriber badges.
- Links are clickable on every platform and open in the browser. Hovering one shows a preview with the page's title, description and picture, fetched only while hovering and never for local addresses; "Link previews" in Settings turns previews off.
- A poll form (question, 2 to 5 choices, length) for Twitch.
- Accounts: connect Twitch with a device code, YouTube through the browser, and pick a Kick channel to read. Tokens are kept in the system's credential store.
- Twitch: chat, deletions, clears, and subs, gifts, raids and announcements (as event lines) over EventSub, sending and replies, moderation and polls, with automatic token refresh and reconnects.
- YouTube: the live chat of your current stream, read over YouTube's streaming chat so it uses a few quota units an hour instead of thousands (polling takes over if the stream isn't available), plus sending, deleting, timeouts, bans and unbans, with a warning before the daily API quota runs out.
- Kick: chat for any channel, without an account, over whichever chat socket Kick offers. Signing in through Kick's official API adds sending, replies, deleting messages, timeouts, bans and unbans; reading your own channel starts by itself after sign-in.
- Connecting an account opens the sign-in page in the browser straight away; Accounts keeps Open page and Copy link for using another browser.
- Settings, with a switch for each platform: a platform turned off disconnects and disappears from the chat, the header and Accounts, keeps its sign-in, and stays off after a restart.
- "Show deleted messages" in Settings: deleted and cleared messages keep their text, faded and struck through, instead of "message deleted".
- Rumble: read-only live chat, Rants, follows and subscriptions through Rumble's Live Stream API, connected by pasting the private link from Rumble's account settings; the link is kept in the system's credential store.
- 7TV emotes: the channel's set on Twitch and Kick and 7TV's global set everywhere, animated, at their own sizes, with zero-width emotes drawn over the emote before them. Emotes the streamer adds, removes or renames update live. BetterTTV and FrankerFaceZ emotes too: the channel's sets on Twitch and their global sets everywhere.
- Emote suggestions: typing `:` and the start of a name in the composer lists the emotes you can use on the platforms the message goes to (your Twitch emotes including subscriptions, Kick's, and 7TV, BetterTTV and FrankerFaceZ), and Tab or Enter fills one in. A `:` alone lists every emote, the channel's own first; long lists scroll with the mouse wheel, the arrow keys, PgUp and PgDn. Tab after the start of a word steps through the emotes that start with it, alphabetically, without a list. Kick emote names in sent messages show as emotes. Your own messages show their emotes in Rill too, from the emote lists as you send and from the platform's copy once it arrives.
- `@` mentions: typing `@` in the composer lists the people who have chatted since Rill started, on the platforms the message goes to, alphabetically; Tab or Enter fills one in. Tab completion on a plain word steps through these names along with the emotes.
- Each kind of event has its own highlight color and switch (subscriptions, gifted subs, raids, Bits/Rants/Super Chats, redemptions, follows, announcements, ad breaks), and any color can be set by hex code.
- First-time chatters are highlighted: Rill remembers who has chatted in your channels and, after watching a platform for a week, marks a new name's first message.
- Highlights: mentions of you (red) and events such as subscriptions, gifted subs, raids, Rants, redemptions and ad breaks (purple) stand out with a colored bar and tint, and thin marks along the scroll bar show where they are in the history. Each kind can be turned off or given another color in Settings, and the marks hidden.
- Long panels such as Settings scroll, with a scroll bar, instead of pushing the chat off the window.
- Text size, 90 % to 150 %, in Settings or with Ctrl+=, Ctrl+- and Ctrl+0; emotes, badges and icons scale with the text.
- Ctrl+F (Cmd+F on macOS) searches the feed: only messages whose text or author matches stay visible, with a count. Esc closes the search.
- Keyboard use: F6 moves between the composer and the messages; arrow keys pick a message and Enter opens its user card; Tab and Shift+Tab move between buttons; Esc closes a card and returns to the composer, and Esc then Tab leaves the composer.
- Rill reopens at the size and place it was closed (kept in `window.json` beside `settings.json`), unless that spot is no longer on a screen.
- An app icon (a chat bubble with three streams in the platforms' colors) for the window, the taskbar and Explorer.
- `--sim` shows simulated chat, with options for rate, bursts, emotes, failures and dropped connections.
