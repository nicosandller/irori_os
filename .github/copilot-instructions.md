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
- **Check files before claiming what they contain.** Quote the actual line, for example the
  value in a fixture, rather than inferring it.

## What's most useful

- Cases where the Rust types in `crates/irori-types` and the generated JSON Schemas in
  `schemas/` would accept different inputs. They must agree; see `fixtures/README.md`.
- Error messages that don't name the field and what's allowed (`docs/specs/entities.md` §7).
- Security issues in `dev/pi review`, the workflows, and anything network-facing.
