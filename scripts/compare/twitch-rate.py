"""Counts chat messages per second in Twitch channels, read anonymously over Twitch's IRC.

python twitch-rate.py <seconds> <channel> [<channel> ...]
"""
import socket, sys, time, collections
chans = sys.argv[2:]; secs = float(sys.argv[1])
s = socket.create_connection(("irc.chat.twitch.tv", 6667))
s.sendall(b"NICK justinfan54321\r\n" + b"".join(f"JOIN #{c}\r\n".encode() for c in chans))
counts = collections.Counter(); buf = b""; start = time.time(); s.settimeout(1)
while time.time() - start < secs:
    try: data = s.recv(65536)
    except socket.timeout: continue
    buf += data
    *lines, buf = buf.split(b"\r\n")
    for l in lines:
        if l.startswith(b"PING"): s.sendall(b"PONG :tmi.twitch.tv\r\n")
        p = l.split(b" ")
        if len(p) > 2 and p[1] == b"PRIVMSG": counts[p[2].decode()[1:]] += 1
for c in chans: print(f"{c}: {counts[c]/secs:.1f}/s")
