# Changelog

## v0.0.0 (unreleased)

The first release of Rill: Twitch, YouTube, Kick and Rumble chat in one fast window, with one
message box that sends to every platform or just one. For Windows 10 and 11.

### Added

- **One feed for every chat.** Messages from Twitch, YouTube, Kick and Rumble arrive in order, each with its platform's icon, the time, badges, the name in the viewer's color, and emotes, animated where they are. Scrolling up pauses the feed; a button jumps back to live chat.
- **Emotes from everywhere.** Each platform's own emotes, plus 7TV, BetterTTV and FrankerFaceZ, channel sets and global ones. 7TV changes show up live. Hovering an emote shows it bigger, with where it comes from.
- **One composer for every platform.** Tab switches between all platforms (a rainbow outline) and one (its color). Each send shows a ✓ or ✗ per platform, and a message too long for any destination isn't sent anywhere. Type `:` for emotes and `@` for the people in chat; Tab completes both.
- **Moderation from one place.** Click a name for a card with that person's messages and what their platform allows: delete, timeouts, bans and unbans. `/nuke` times out or bans everyone who recently said a phrase, on every platform at once, after showing how many it would hit; `/unnuke` undoes it.
- **Commands.** `/ban`, `/timeout`, `/unban`, `/purge`, `/title` and `/category` (on every platform that can), `/ad`, polls, Twitch chat modes, `/announce`, `/shoutout`, `/search` and more. Type `/` for the list.
- **Events and highlights.** Subscriptions, gifted subs, raids, Bits, Rants and Super Chats, redemptions, follows, announcements and ad breaks show in the feed, each kind in its own color, along with mentions of you. Marks on the scroll bar show where they are. Every color can be changed, down to any hex code, and first-time chatters can be highlighted too.
- **Links, badges and copying.** Links open in your browser, and hovering one shows a preview (which Settings can turn off). Hovering a badge says what it is, including how long someone has subscribed. Drag across messages to copy them with their times.
- **OBS overlay.** Show your chat on stream as an OBS Browser source, with its own text size, fading, badges, backing, highlights, and hiding of commands and bots. It runs on your computer only, and only while it's turned on.
- **Signing in.** Twitch, YouTube and Kick sign in through your browser; Rumble takes the link from your Rumble account. Kick can read any channel without signing in. Sign-ins are kept in Windows Credential Manager, and only on your computer.
- **Settings.** Turn platforms off, choose how much chat history to keep (with what each choice costs in memory), text size from 90 % to 150 %, deleted messages shown faded instead of removed, and more. Rill reopens where you left it.
- **Keyboard.** F6 moves between the composer and the messages; the arrow keys pick a message, Enter opens its card, and Esc goes back. Ctrl+F searches the feed.
- **Installs and updates itself.** The installer needs no administrator rights and adds Rill to the Start menu and Windows' list of apps. Rill downloads new versions in the background and installs them the next time it starts, or at once with Update.
- **Help when something goes wrong.** If Rill hits a problem, the next start offers to send a report to the Rill team, and Settings can send the recent log at any time. Nothing is sent unless you choose to, and never chat messages or passwords.
