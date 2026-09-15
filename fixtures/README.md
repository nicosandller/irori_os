# Golden fixtures

Example documents that the tests check against **both** the Rust types in `irori-types` and
the generated JSON Schemas in `schemas/`, so the two can never quietly disagree.

```
fixtures/types/<schema>/valid/*.json      must parse, pass the schema, and round-trip
fixtures/types/<schema>/invalid/*.json    must be rejected, with the message in *.error.txt
```

Files people write by hand are TOML (`extension-manifest/*.toml`). They're read into the JSON data
model first, the way Irori reads them, and then checked the same way.

- `<schema>` matches a file in `schemas/` (`entity-state` → `schemas/entity-state.schema.json`).
- Every invalid `name.json` has a `name.error.txt` holding a substring of the expected error. Error
  messages are part of the contract: people and LLMs fix their input based on them.
- An invalid file named `*.schema-allows.json` (or `.toml`) breaks a rule that JSON Schema can't express
  (e.g. comparing two timestamps). Only Rust rejects it, and the test asserts the schema
  accepts it, so if the schema ever learns the rule, rename the file.

Run with `cargo test -p irori-types`.
