# Spec: backup and restore

Status: **partly shipped** (ROADMAP C29). Shipped: the archive, `irori backup create|list|restore`,
the three API routes, and scheduled backups. Not yet: restoring from the page, and a Settings row.

A backup is one file that holds a home: what you said about it (`config/`), what Irori
remembered (`irori.db`, extension state), and nothing that can be fetched again.

## 1. The file

`irori-backup-<UTC yyyymmdd-hhmmss>.tar.gz`, mode `0600`, in a directory of mode `0700`. Three
roots, and no absolute paths, so a backup restores into any `--config` and `--data`:

```
manifest.json   { format, irori_version, created_at, extensions }
config/         the whole config directory, secrets.toml, users.toml and flows/ included
data/           irori.db, extension-data/, tls/ and any other state
```

**Left out:** `ollama*` (models are gigabytes), `extensions/` (installed packages are fetched
again; `manifest.json` lists their ids so a restore can say which), `cli.url`, logs and pids, the
`-wal` and `-shm` files, and the backup directory itself.

`irori.db` is never copied as a file: it is in WAL mode with several connections open. It is
written with `VACUUM INTO` on a read-only connection, which is consistent and does not stop the
server. A backup is safe to take while Irori runs.

## 2. Where they go

`<data>/backups`, or `[backup] dir` (relative to the data directory). A file is written as
`.part` and renamed, so a listing never shows half of one.

## 3. Schedule

```toml
[backup]
enabled = false     # take one on a schedule; creating one by hand works either way
every_hours = 24    # at least 1
keep = 7            # newest irori-backup-* files kept; older ones are deleted after each new one
# dir = "backups"
```

Read at startup. On start, if the newest backup is older than `every_hours` (or there is none),
one is taken after a minute. A failed backup is logged and tried at the next time.

## 4. Restore

Restore replaces files the server has open, so it is **offline only**: `irori backup restore`
refuses when a server answers at `<data>/cli.url`. It is also the path for a dead or new machine.

Everything that can be refused is refused before the first file changes:

1. Every member of the archive must be a plain file or directory under `manifest.json`,
   `config/` or `data/`: no links, no absolute names, no `..`.
2. `manifest.json` must be `format` 1 and not from a newer Irori than this one (a `0.0.0`
   development build reads anything).
3. The archive is unpacked into a staging directory; its database must pass
   `PRAGMA integrity_check` and its config must load without problems.

Then the home as it is now is saved as `pre-restore-<time>.tar.gz` in the backup directory (not
pruned), the config directory is replaced whole, and `irori.db` and the other state in the
archive replace theirs. Packages and models in the data directory are not touched. On a machine
with no home yet there is nothing to save.

## 5. Surfaces

- CLI (`docs/specs/cli.md`): `irori backup create [--to <dir>] [--prune]`, `list`, `restore
  <file|name> [--yes]`. They work on files, not over the API.
- API: `GET /api/backups`, `POST /api/backups` (also prunes to `keep`), `GET /api/backups/{name}`
  (the file; only names this module writes are served).

## 6. Who may

A backup holds password hashes and secrets. The routes are not in the read list, so only the
owner's session with the page's `x-irori-ui: 1` header gets in; a token never does
(`docs/specs/api.md` §1).

## 7. Not here yet

Restore from the page (stage, then apply at startup before the database opens), encryption of
the file, and destinations other than a local directory.
