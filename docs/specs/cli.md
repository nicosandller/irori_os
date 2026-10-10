# Spec: the command line

Status: **shipped** (ROADMAP C22).

`irori` is one binary. `irori serve` (also `irori run`) is the server. Every other command
talks to a server that is already running, over the public API (`docs/specs/api.md`), or
removes what `install.sh` put on this machine.

The page is still where you draw a floor plan, pick a city on the map, turn motion on and
off, and edit a flow on the canvas. The CLI reads and changes the same home those pages do.

## Which server

A remote command finds Irori in this order:

1. `--url`, or `IRORI_URL`.
2. `<data>/cli.url` when `--data` is set. If that file is missing, the command stops. It
   does not guess port 8480, which might be a different Irori.
3. `./data/cli.url`, when that file is there.
4. `http://127.0.0.1:8480`.

`serve` writes `<data>/cli.url` after it binds. A bind of `0.0.0.0` or `::` is written as
`127.0.0.1` or `::1`, so a CLI on the same machine can connect. `--url` wins over the file.
The file is removed when the server stops. A restart leaves it, and the new process
replaces it.

`--data` on a remote command is that directory. Put it before the subcommand.
`entities call` uses `--data` for the action's JSON.

```sh
irori --url http://127.0.0.1:8480 status
irori devices --data /var/lib/irori list
```

## Who is calling

| | |
|---|---|
| `--token`, `IRORI_TOKEN` | A program token. Reads and service calls its scopes allow. Setup changes are refused, and the CLI adds: sign in with `irori login`. |
| `irori login` | The page's session. The password is asked (or read with `--password-stdin`) and is not saved. The cookie goes in `$XDG_CONFIG_HOME/irori/session`, or `~/.config/irori/session`, mode `0600`. It is used only when its origin matches this server. |

A session is the owner (or whoever signed in) for the API's rules. The CLI sends
`x-irori-ui: 1` on changes made with that cookie, because it is Irori's own program, the
same way the page does. A bearer token never sends that header. Tokens do not gain the
ability to install extensions, mint tokens, or restart.

`irori logout` deletes the session file and asks the server to forget the session.

On an open home, with no owner yet, commands that only need to reach the server work
without a login. `restart` still sends `x-irori-ui`, which that route always requires.

## Flags on every remote command

`--url`, `--token`, `--insecure`, `--ca`, `--json`, `--yes` can sit on the command or on
a subcommand. `--data` sits on the command, before the subcommand.

`--insecure` skips certificate checks, for the certificate Irori made itself.
`--ca <pem>` trusts that file as well as the system roots.

`--json` prints one pretty JSON document on stdout. A failure prints
`{ "error": "…" }` and exits 1. Clap's own refusal of a bad invocation stays exit 2.

A destructive command asks on a terminal. Without a terminal, or with `--json`, it
refuses unless `--yes` is set. `--yes` does not skip the full-access sentence.

## Commands

`irori help <command>` lists the rest.

| Command | What it does |
|---|---|
| `serve` / `run` | The server. |
| `version` | The build. `--json` for a machine. |
| `completions <shell>` | A completion script for the named shell. |
| `uninstall` | What `install.sh` put down. See below. |
| `login` / `logout` / `status` | Sign in, sign out, and whether this Irori is up. |
| `devices` | List, show, add, rename, move to a room, describe, remove. |
| `entities` | List, show, rename, history, summary, call. |
| `areas` / `floors` | Rooms and levels. |
| `floorplan` | `get` prints the plan as JSON. `put <file>` replaces it. Drawing stays on the page. |
| `watch` | State changes until Ctrl-C. |
| `extensions` | Catalog, install, install from a URL, uninstall, settings, secrets, actions, log. |
| `helpers toggle` | Add or remove a toggle Irori keeps itself. |
| `token` | List, create, revoke. The secret is printed once. |
| `users` | List, add, edit, remove. |
| `place` | Show, or set coordinates, an optional label, and a time zone. No map search. |
| `recorder` | How long history is kept. |
| `logs` | What this Irori has said since it started. |
| `restart` | Start this Irori again. It stays where it runs. |
| `automations` | Flows. Also `rules`. The extension has to be installed. |
| `assistant` | Only in a build with the assistant. |

`entities call <id> <command>` posts `/api/command`. `on`, `off`, and `toggle` call
`turn_on`, `turn_off`, and `toggle`. `--data` is a JSON object, the action's data.

`entities history <id>` is the last day. `--since <timestamp>` keeps later rows of that
day. Older readings are `entities summary <id> --since <timestamp>`.

`devices remove` unpairs a Zigbee device first. If the network doesn't answer, the error
names `unpair_failed` and the device stays. `--force` removes it anyway.

`extensions install` prints the full-access sentence when the package has it, in the same
words as the page: full access to this machine, and that it can download and run other
programs, the same as a terminal. The install is sent only after a yes, or when
`--approve-full-access` was passed. `--yes` alone does not approve it.
`extensions uninstall` with no id is a numbered menu on a terminal. There is no `--all`.
`--json` with no id refuses, and asks for an id.

`automations validate` reads a flow JSON file, or the saved flow when you pass its id and
no file. Errors exit 1. Saving a flow and the canvas stay on the page.
`automations test --dry` does not touch the home.

`watch` opens `/api/ws`. `--insecure` with an `https` URL is refused: use `http` on this
machine, or trust the certificate with `--ca`.

`place set` takes `--latitude`, `--longitude`, `--timezone`, and an optional `--label`.
The label is the place's name, not a name for the home.

## `irori uninstall`

This does not call the API. On a terminal it is a menu. Without one, pass `--binary`,
`--unit`, or `--purge <dir>`.

- `--binary` removes the binary only when it is the one `install.sh` put in
  `~/.irori/bin/irori` or `/usr/local/bin/irori`, and removes the `# irori` PATH line that
  follows it in the shell config. A development binary is left where it is.
- `--unit` disables and deletes `/etc/systemd/system/irori.service` when it is there.
- `--purge <dir>` deletes that directory. `/` and your home directory are refused.

`--yes` skips the question. It does not choose what to remove.
