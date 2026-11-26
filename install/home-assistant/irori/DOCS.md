# Irori

Irori runs beside your Home Assistant, on the same machine. Home Assistant keeps running your
home. Irori reads it through its Home Assistant extension, and passes on the commands you give
in Irori unless you turn that off.

## Start

1. Install the app and press **Start**. Irori keeps its whole home (settings, database,
   extensions) in this app's data folder, so it's in Home Assistant's backups. Irori is stopped
   while a backup is made, because its database must not change under it.
2. Press **Open web UI**. Irori opens in its own browser tab on port 8480.
3. **Set up the owner first.** The welcome asks who you are, with a password. Until then Irori
   is open to anyone on your network who finds port 8480, so do this straight away.

## Connect Home Assistant

1. In Irori, open **Extensions** and install **Home Assistant**.
2. In its settings:
   - **Leave "Home Assistant address" empty.** Irori then uses the Home Assistant on this
     machine, at `http://127.0.0.1:8123`. The token never leaves the machine.
   - **Access token**: in Home Assistant, as an administrator, open your profile → Security →
     Long-lived access tokens → Create token, and paste it.
   - Turn off **Let Irori control devices** if you only want to look.
3. Bring over Home Assistant's floors and rooms, and add its devices, from the extension's page.

If your Home Assistant serves https, or listens on a port other than 8123, enter its address
instead, for example `https://127.0.0.1:8123`, and for a self-signed certificate its fingerprint
under "Trust this certificate".

## What this app doesn't do yet

- **You still paste a token.** Home Assistant gives every app a token of its own, but Irori's
  Home Assistant extension can't use it yet: that token only works through the Supervisor, at an
  address the extension doesn't accept today.
- **Irori isn't in Home Assistant's sidebar.** It opens in its own tab. Showing it inside Home
  Assistant needs Irori's page to work under another address, which it doesn't yet.
- **The port is fixed at 8480.** The app uses the host's network, so changing the port in the
  app's Network settings does nothing.

## Updates

New Irori versions arrive in Home Assistant's own update list. Irori's Settings page may also say
a newer release exists: update through Home Assistant, not from Irori. After an update, the
Extensions page offers each installed extension its newer version.

## Radios and Bluetooth

This app has no access to USB radios or Bluetooth: those stay Home Assistant's. Use Irori's
Home Assistant extension to see the devices behind them.
