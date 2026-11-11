# Changelog

## v0.0.2 (unreleased)

Fixes and polish from the first test stream.

### Changed

- Moderation reads as one line per action, once the platform has done it, such as
  `Mobasity banned Foo. Reason: spam`, `ModFoo timed out Bar for 10m.` or `Mobasity VIP'd Bar.`
  Other moderators' actions show too where the platform reports them (Twitch and Kick), and the
  "working" and ✓ lines are gone. A `/nuke` or `/meganuke` is one line for everyone it caught.
  The overlay never shows these lines; it only removes the messages.
- Twitch asks for five more read permissions, for moderation events. A Twitch sign-in from an
  earlier version keeps working, and Rill asks once to reconnect Twitch to get them.
- A banned or timed-out user's messages disappear as soon as the platform confirms, instead of
  when its own event arrives, which on YouTube took a while.
- The user card has Unban, a row of timeouts from 1 second to 1 week, and Ban, always shown, with
  buttons that light up under the pointer and when pressed. Unban lifts timeouts too. The card
  also copies the user's name and ID.
- Times, badges and "only messages that mention you" are switches in Settings instead of header
  buttons, and they're remembered. A narrow window still hides times and badges by itself.
- Settings → Names picks how names look, separately for Rill's window and the overlay. Simple
  shows only the platform's icon and colors names by role: the owner orange-red, moderators red,
  VIPs pink, subscribers light blue and everyone else white, each color changeable. Badges shows
  owner, moderator, VIP and subscriber badges and each name's own color, and replaces the Show
  badges switch. Rill's window starts in Badges and the overlay in Simple.
- Kick's broadcaster, moderator, VIP and subscriber badges look as they do on Kick, instead of
  lettered squares. YouTube's and Rumble's role badges are small icons, and YouTube names take
  YouTube's own colors for the owner, moderators and members.
- Rumble is off until turned on in Settings.
- Messages sent to every platform, and lines about several platforms or none, show Rill's logo.

### Fixed

- YouTube's own emoji, such as `:hand-pink-waving:`, show as pictures instead of their names.
  A channel's member emoji still show as text: YouTube doesn't give apps their pictures.
- After signing in to YouTube while not live, Accounts kept saying to finish signing in.
- After approving Twitch's sign-in code, Accounts showed the sign-in prompt until Twitch connected.
- Copy buttons say "Copied" for a moment, so a click shows.
- The overlay drew no broadcaster, moderator or VIP badge on Kick, and no badges on messages sent
  from Rill.
- Hovering a command in the `/` list shows its whole description when the window cuts it short.
- Settings explains how to size the overlay in OBS: dragging the source's corners only stretches
  it, while its Width and Height give chat more room.

## v0.0.1 (2026-11-09)

The first version of Rill with an installer. Everything listed under v0.0.0 is in it: v0.0.0 was
never built, because the installer tool needs a version of 0.0.1 or later.

### Fixed

- Rill's installer and update files are built for this version; v0.0.0 has none.

## v0.0.0 (2026-11-09)

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
- **Signing in.** Twitch, YouTube and Kick sign in through your browser; Rumble takes the link from your Rumble account. Kick can read any channel without signing in. Sign-ins are kept in Windows Credential Manager, and only on your computer. YouTube is limited for now: until Google finishes reviewing Rill, only Google accounts on Rill's test list can connect it, and its sign-in lasts 7 days.
- **Settings.** Turn platforms off, choose how much chat history to keep (with what each choice costs in memory), text size from 90 % to 150 %, deleted messages shown faded instead of removed, and more. Rill reopens where you left it.
- **Keyboard.** F6 moves between the composer and the messages; the arrow keys pick a message, Enter opens its card, and Esc goes back. Ctrl+F searches the feed.
- **Installs and updates itself.** The installer needs no administrator rights and adds Rill to the Start menu and Windows' list of apps. Rill downloads new versions in the background and installs them the next time it starts, or at once with Update.
- **Help when something goes wrong.** If Rill hits a problem, the next start offers to send a report to the Rill team, and Settings can send the recent log at any time. Nothing is sent unless you choose to, and never chat messages or passwords.
