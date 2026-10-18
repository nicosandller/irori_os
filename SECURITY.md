# Security

Irori runs the home: it switches locks and alarms, holds device keys, and installs extensions
that run on the machine. A flaw in it can matter outside the screen, so please report one
privately and give a fix time to ship before anything is public.

We'll reply within a few days. We aim to fix a confirmed vulnerability within 90 days; after
the fix ships, or once 90 days have passed, you're free to share it publicly.

## Reporting a vulnerability

Use **Report a vulnerability** on this repository's
[Security tab](https://github.com/nicosandller/irori_os/security/advisories/new). Only the
maintainers see the report, and the advisory, the fix and the credit are worked out there.

Please don't open a public issue, pull request or discussion for a vulnerability. Ordinary bugs
and hardening that isn't exploitable go in issues as usual.

A useful report says which release or commit (`irori version`, or the version shown in the web
page), how Irori was run (the flags, or the container), what an attacker needs (on the LAN, a
page in the owner's browser, an installed extension, a signed-in user), and the steps that show
it.

## What's supported

Fixes go into the next release, and only the latest release is supported. Irori checks for new
releases and offers them, but you need to install the update yourself (and the check can be
turned off), so an older install gets the fix only once its owner updates.

## In scope

- The `irori` binary and the crates it's built from, including the web page (`crates/irori-ui`):
  the HTTP API, sign-in, tokens and sessions, the extension host and package installs,
  automations, and the assistant (`extras/irori-assist`).
- The official extensions listed in `extensions/official.toml`, except the demo, wherever the
  package is installed from (this checkout, packages shipped beside the binary, or a release).
- `install/install.sh` and the CI and release workflows in `.github/workflows`.
- Third-party extensions are their authors' to fix, but a way for one to escape what it was
  granted is in scope here.

## Not in scope

- The demo extension (`extensions/demo`) and the development tools in `dev/` and `xtask/`.
- Someone who already has physical or root access to the machine.
- Someone on the LAN when the owner chose to open the port to it.
- An extension the owner installed doing what it was allowed to do.
- Denial of service.
- Bugs in other people's libraries, unless Irori uses them in an unsafe way.
