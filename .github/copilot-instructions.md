# Instructions for Copilot code review

Irori is a Rust smart home core. The project owner reviews your comments, so precision matters
more than volume: one verified finding beats five guesses.

## Before reporting

- **CI decides whether code compiles.** Don't report compile errors, type errors, or borrow
  checker problems (e.g. "this moves out of `&self`") unless the CI run for that commit failed.
  CI runs `cargo fmt --check`, `cargo clippy -D warnings`, and `cargo test` on every pull request.
- **Only enabled lints count.** Lints are configured in `[workspace.lints]` in `Cargo.toml`.
  Don't report lints from groups that aren't enabled there, such as `clippy::pedantic`
  (`unused_async`, …).
- **Don't repeat resolved threads.** If a thread on the same code was answered and resolved,
  don't raise the same point again unless the code changed.

## Decisions already made (don't flag these)

- `irori-core` depends on `irori-integration`. That crate is the integration SDK (the
  `Integration` trait), not an integration; the core's integration host needs it. What
  ROADMAP §2.1 forbids is `irori-int-*` crates and protocol libraries, which
  `cargo xtask check-deps` enforces.
- `rustup toolchain install` with no arguments, in `dev/Dockerfile`, installs the toolchain
  named in `rust-toolchain.toml`. It's intentional and works.
- CI's musl cross-builds get their C compiler (for bundled SQLite) from `cargo-zigbuild`,
  which sets `zig cc`. No separate musl GCC is needed.
- `dev/pi down --wipe` uses `docker volume rm --force`, which succeeds when the volume
  doesn't exist.
- CI's `push` trigger covers only `main`, on purpose: pull requests run through the
  `pull_request` trigger, so branches don't run twice.
- JSON Schema `pattern`s follow ECMA-262, where `$` (no `m` flag) matches only at the end of
  input, never before a trailing newline. Don't flag `$` anchors in `schemas/` or in the
  schema code in `crates/irori-types`. Python's `re` behaves differently, but it isn't the
  reference, and Rust rejects those inputs when data enters Irori anyway.
- Numbers are compared as IEEE 754 doubles, the way mainstream JSON Schema validators (JS,
  Python, Rust `jsonschema`) read them. Don't flag literals whose extra digits are lost in
  that conversion (e.g. `1.0000000000000001` reading as `1`); Irori doesn't do
  arbitrary-precision parsing.
- **Built-in integrations may not block, and the core doesn't isolate them from that.** They
  run as Tokio tasks on the shared runtime on purpose: they're first-party code shipped with the
  core, and the contract (`docs/specs/integrations.md` §3) says they must not block. Third-party
  code runs as a separate process instead. Don't propose per-integration runtimes or threads for
  the built-ins; the cost on a Raspberry Pi isn't worth a rule we already enforce by review.
- **`last_reported` starts when an entity is registered, and is never null.** Describing an
  entity is the integration telling Irori about it, and "has never reported a value" is already
  visible as `state: null` (`docs/specs/entities.md` §5.1). Don't propose making the field
  optional or adding a sentinel for entities that have only been described.
- **The per-entity call turnstile is best-effort ordering, not mutual exclusion.** Two commands
  can overlap at a device when a caller gives up mid-call. What keeps `toggle` correct is the
  remembered command in `Home::commanded` (`docs/specs/integrations.md` §7.1), which survives a
  cancelled caller. Don't propose moving call supervision off the caller's future to close that
  window; the restructuring isn't worth it for a cancelled command arriving a moment early.
- **`crates/irori-ui` is outside the Cargo workspace on purpose.** It builds for
  `wasm32-unknown-unknown` with `trunk` (`cargo xtask ui`), so keeping it out means a plain
  `cargo build` needs no wasm toolchain and the workspace's native builds don't drag in a
  browser framework. CI's `ui` job runs its own fmt, clippy and build. Don't propose adding it
  to `members`.
- **The binary embeds two asset folders, and that's the point.** `crates/irori/assets/` is the
  placeholder page that any `cargo build` can serve; `crates/irori/ui/` is the built Leptos app,
  which wins when it's there (`crates/irori/src/server.rs`). Don't propose merging them or
  failing the build when the UI hasn't been built.
- **The UI polls `/api/dev/home` every 2 seconds.** `/api/dev/*` is a temporary, unauthenticated
  API, and pushing changes waits for the WebSocket API in M1.5. Don't propose WebSockets, SSE,
  ETags, or caching headers for it yet.
- **Plaintext ESPHome devices are adopted without authentication, on purpose and knowingly.**
  ESPHome's native API has no device authentication of its own, so `irori-int-esphome` connects
  to whatever announces `_esphomelib._tcp`. This is decision **D29**: the alternatives (an
  allowlist, a record of which devices were adopted, or the encryption keys that make it moot)
  all need somewhere to keep a decision, which is the config dir in M0.7. It's written up in
  `integrations/irori-int-esphome/README.md`, and every device's first connection warns in the
  log. Don't raise unauthenticated adoption, mDNS spoofing, or device-id impersonation again
  until encryption lands. **Still worth reporting:** a flaw in the encrypted path once it
  exists, anything that widens exposure beyond the local network, or a way this reaches past
  ESPHome's own entities.
- **`cargo install --path` doesn't need `--force`.** A path source has no version to compare
  against a registry, so Cargo rebuilds and replaces every time: `Replacing …/bin/irori` /
  `Replaced package \`irori v0.0.0\``. Checked by running it. `--force` matters for installing
  the same version from a registry, which `cargo xtask install` never does. Don't propose adding
  it to `xtask/src/ui.rs`.
- **A device has one id, one name and one description (ROADMAP D36).** Don't suggest showing the
  firmware's or integration's name next to a name a person chose, keeping a "display name"
  separately, or deriving a device id from its name. Device ids are made from the integration and
  its handle on purpose, so a rename can never move them.
- **Check files before claiming what they contain.** Quote the actual line, for example the
  value in a fixture, rather than inferring it.
- **`SqliteStorage` syncs rusqlite behind a `Mutex` on the host task on purpose.** Extension KV
  is tiny and infrequent (helpers toggles, paired keys) — a few writes a minute at most, not a
  stream. Don't propose spawning a dedicated async DB worker for it in this milestone; see the
  comment on `SqliteStorage` in `crates/irori/src/db.rs`.

## What's most useful

- Cases where the Rust types in `crates/irori-types` and the generated JSON Schemas in
  `schemas/` would accept different inputs. They must agree; see `fixtures/README.md`.
- Error messages that don't name the field and what's allowed (`docs/specs/entities.md` §7).
- Security issues in `dev/pi review`, the workflows, and anything network-facing.
