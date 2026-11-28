# irori-protocol-bambu

Bambu Lab printers on the local network, over the printer's own MQTT. Nothing talks to Bambu
Lab's cloud and no Bambu account is used. **Read-only for now:** the extension only reads the
printer. Pausing, resuming, stopping and the light are refused with a message until they have
been tried on real printers.

**Not tried on a real printer yet.** Everything here is tested against a fake printer in the
test code, built from what is publicly documented about the printers' local MQTT.

## What it does

- **Connects to one printer** at the address in the settings, on port 8883, over TLS, and signs
  in with the printer's access code. One printer per installed extension for now.
- **Asks for the whole status once per connection** (`pushall`), then listens. Some printers
  send only the values that changed, so each report is laid over what is already known: a value
  a report leaves out keeps what it had.
- **The printer is one device**, with:
  - what it is doing: `idle`, `preparing`, `printing`, `paused`, `finished` or `failed`
    (unknown for a state the extension doesn't know);
  - progress in percent, remaining time in minutes, the layer and the total layers;
  - nozzle and bed temperature;
  - each slot of each filament unit the printer reports: the filament's type (`empty` for an
    empty slot), with its colour and the percentage left beside it when the printer knows them.
- **Ids come from the serial number**, so they survive a new address.
- **When the connection drops** the device is marked unavailable and the extension's card says
  why. It connects again by itself, waiting 1 second at first and up to 60 between tries, and
  the device is available again at the printer's next report.
- **Everything read is bounded:** a message over 256 KiB drops the connection, at most 12
  filament units of 16 slots are kept, text is cut to 64 characters, and a number out of range
  or of the wrong type is passed over.

## What is trusted

The printer shows a certificate of its own, not one a public authority issued for its address,
so the usual check can't pass. What the extension does instead:

- **The handshake's signature is always checked** against the certificate, so whoever answers
  holds that certificate's key.
- **On the first connection, the certificate shown is taken as it is**, and the access code is
  sent. Anything answering at the printer's address at that moment would get the access code.
  To rule that out, put the certificate's SHA-256 fingerprint in `certificate` before the first
  connection.
- **Once the printer has accepted the access code, its certificate is remembered.** From then on
  the access code only goes to a printer showing that same certificate. If the address answers
  with another one, nothing is sent, the device stays unavailable, and the card shows the
  fingerprint that was offered. If the printer really was reset or replaced, entering that
  fingerprint in `certificate` accepts it.
- **Not checked:** whether Bambu Lab issued the certificate, and whether it names this printer's
  serial number.

The connection uses TLS 1.2 only: the public Home Assistant integration for these printers
notes a firmware that never answers a TLS 1.3 hello.

## Settings

The access code is secret, so it goes in `secrets.toml`; the rest can go in
`extensions/bambu.toml`. The three required settings are shown on the printer's own screen.

```toml
# extensions/bambu.toml
host = "printer.home.arpa"        # or a private IP address
serial = "00TEST0000000001"
# certificate = "<64 hex digits>"  # optional: only ever trust this certificate

# secrets.toml
[bambu]
access_code = "<the access code>"
```

The address has to be on the local network: a private, loopback or link-local IP, or a name
under `.local`, `.home.arpa`, `.internal`, `.lan` or `.home`. Changing the address clears the
access code, so it is never sent to an address it wasn't entered for.

## Not yet

- Pause, resume, stop, the light, and setting the bed and nozzle targets.
- Finding printers by itself, and more than one printer.
- Fans, the door, errors, the chamber temperature, and a filament unit's humidity, temperature
  and drying state.
- Temperatures on printers that report them only in the newer packed form (under `device` in
  the status) show as unknown, as does the second nozzle of a two-nozzle printer.
- A filament unit that is taken off stays listed, with its last values.
- The camera.
