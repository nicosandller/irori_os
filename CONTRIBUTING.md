# Contributing

Irori runs the home, so a change has to be one a reviewer can read and trust. This page covers
what that takes. Vulnerabilities go through [SECURITY.md](SECURITY.md), not a pull request.

## One change per pull request

- A pull request does one thing: a fix, a feature or a refactor, not a mix. If you find
  something else on the way, open an issue or a second pull request for it.
- Say what changed and why in plain words, and link the issue it fixes ("Closes #N", or
  "Part of #N" when it fixes only part).
- A bug fix comes with a test that fails without the fix. Say so in the description.

## Checks

Run the list under [Checks (same as CI)](README.md#checks-same-as-ci) before asking for a review,
and `cargo xtask ui` if you changed the web page. CI runs the same checks on every pull request.

Never weaken a check to make a change pass:

- don't delete, `#[ignore]` or loosen an existing test or assertion;
- don't add `#[allow(...)]` to quiet a clippy warning;
- don't drop or relax a CI step or budget (the UI size limit, `-D warnings`, `--locked`).

If a test or budget really is wrong, change it in its own commit and say why in the
description, so the reviewer decides on it separately.

## AI-assisted work

AI-assisted contributions are welcome. You remain the author: you've read every line you
submit, you can explain it in review, and you've run the checks yourself. Large amounts of
generated code that nobody has checked will be closed. The rules above apply the same way:
an assistant doesn't get to skip or weaken a test either.

## License

Contributions are accepted under the project's license, Apache-2.0 ([LICENSE](LICENSE)).
