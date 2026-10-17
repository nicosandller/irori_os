# Security

Irori runs the home: it switches locks and alarms, holds device keys, and installs extensions
that run on the machine. A flaw in it can matter outside the screen, so please report one
privately and give a fix time to ship before anything is public.

## Reporting a vulnerability

Use **Report a vulnerability** on this repository's
[Security tab](https://github.com/nicosandller/irori_os/security/advisories/new). Only the
maintainers see the report, and the advisory, the fix and the credit are worked out there.

Please don't open a public issue, pull request or discussion for a vulnerability. Ordinary bugs
and hardening that isn't exploitable go in issues as usual.

A useful report says which release or commit (`irori version`), how Irori was run (the flags,
or the container), what an attacker needs (on the LAN, a page in the owner's browser, an
installed extension, a signed-in user), and the steps that show it.

## What's supported

Fixes go into the next release. Irori moves quickly, and an install made with `install.sh`
updates itself, so only the latest release is supported; an older one gets the fix by
updating.

## In scope

The `irori` binary and the official extensions in this repository: the HTTP API and the page,
sign-in, tokens and sessions, the extension host and package installs, the protocol extensions,
automations, the assistant, `install.sh`, and the release workflow.
Third-party extensions are their authors' to fix, but a way for one to escape what it was
granted is in scope here.
