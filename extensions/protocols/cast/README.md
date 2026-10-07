# irori-protocol-cast

Google Cast receivers on the local network: Chromecast, Nest speakers, and speaker groups. One
media player each. There is no Google account and no call off the LAN. The receiver fetches
whatever it is asked to play.

## What it does

- **Finds receivers** by mDNS (`_googlecast._tcp`). `known_hosts` adds addresses that don't
  announce themselves; each one is read from `http://{host}:8008/setup/eureka_info`.
- **Adopts all of them**, or only the ids in `uuids`.
- **Reports playback**: idle, playing, paused, buffering, off, or standby. Netflix runs on the
  receiver itself and never says what it's doing, so Netflix in front is shown as playing.
- **Plays an http or https URL** on the Default Media Receiver. `turn_off` quits the running app.
- **For a TV, `turn_on`** launches that same receiver with nothing loaded, which is what lets a
  Chromecast stick ask the TV to switch to its input.
- **Keeps the player** when the socket drops, marks it unavailable, and connects again (1 s,
  doubling up to a minute).

## Settings

```toml
[cast]
known_hosts = ["192.168.1.20", "tv.local"]
# 32 lowercase hex characters, no dashes. Empty adopts every receiver.
uuids = []
# A device id, or the receiver's exact name. Skips HDMI standby for that one.
ignore_cec = []
```

A known host has to be a private, loopback, or link-local address, or a name under `.local`,
`.home.arpa`, `.internal`, `.lan`, or `.home`. A port, a public address, or an internet name is
refused.

## Playing something

Cast devices resolve names with Google's DNS (`8.8.8.8` and `8.8.4.4`). The URL has to be an IP
address or a name the public DNS can answer. A `.local` name will not play, even when this
machine can open it.

## A note on trust

The certificate a receiver presents is not in the public web PKI, so it can't be checked against
the usual roots. The session accepts that certificate and still verifies the TLS handshake
signature. Anything on the LAN that answers as a Cast device is believed, the same limit as a
plaintext device that announces itself.

## HDMI standby

`isStandBy` and `isActiveInput` are read from the receiver's own status, and only when that
status includes the field. A missing `isStandBy` is not standby. Speakers, groups, and anything
listed in `ignore_cec` skip both flags.

Standby wins over "the TV is on another input". Nothing this protocol sends puts the TV into
standby: `turn_off` only quits the app, and `turn_on` only launches the default receiver.

## Left out

YouTube, Plex, and the other named apps, joining or leaving a speaker group, and casting a
dashboard. A receiver plays an http(s) URL and reports what it is doing; that is the whole
surface.
