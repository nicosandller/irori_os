# Spec: backup and restore

Status: **shipped** (ROADMAP C29).

A backup is one file that holds a home: what you said about it (`config/`), what Irori
remembered (`irori.db`, extension state), and nothing that can be fetched again. It can be made
by hand, on a schedule, or just before an update; copied to other places; and put back from the
page, from a terminal, or in place of setting up a new home.

What Home Assistant's backups give (a schedule with retention, encryption with a key you keep,
several locations, restore from the page and from the first-run screen, a backup before an
update) is all here. What is here and isn't there: a restore of only some parts, with history
left as it is; a check that opens a backup without restoring it; nothing leaves the machine
unencrypted; a reading on Irori's own device that says when backups have stopped.

## 1. The file

`irori-backup-<UTC yyyymmdd-hhmmss>.tar.gz`, or `….tar.gz.enc` when encrypted. Mode `0600`, in a
directory of mode `0700`. Three roots, and no absolute paths, so a backup restores into any
`--config` and `--data`:

```
manifest.json   { format, irori_version, created_at, extensions, encrypted, history }
config/         the config directory, secrets.toml, users.toml and flows/ included
data/           irori.db, extension-data/, tls/ and any other state
```

**Left out:** `ollama*` (models are gigabytes), `extensions/` (installed packages are built for
one CPU and fetched again; `manifest.json` lists their ids), `cli.url`, logs and pids, the
`-wal` and `-shm` files, the backup directory itself, and dot-entries (`.git`) at the top of
`config/` and `data/`: the config directory is meant to be kept in git, and that history is
the person's, not Irori's.

`irori.db` is never copied as a file: it is in WAL mode with several connections open. It is
written with `VACUUM INTO` on a read-only connection, which is consistent and stops nothing, and
the copy is checked (`PRAGMA quick_check`) before it goes in. A backup is safe to take while
Irori runs. With `include_history = false` the five history tables are emptied in the copy.

Backups made by hand or on the schedule are `irori-backup-…`; the one made before a restore is
`pre-restore-…` (the last three are kept); the one made before an update is `pre-update-…` (the last
three are kept). Two in the same second get `-2`.

## 2. Encrypted

Unless turned off, a backup is encrypted with a **recovery key**: 25 characters of Crockford base32
in groups of five (125 bits), made once and kept in `secrets.toml` under `[backup]`. Anything
else of at least 12 characters can be set as the key instead. A key is read without its dashes or
its case.

```
"IRORIBK" 0x01 | u32 header length | header JSON | chunk | chunk | …
```

The header is public: the format, the key derivation (argon2id, its parameters and a random
salt), the stream's nonce, and what a listing shows (version, time, extensions), so backups can
be listed without the key. The body is the gzipped tar in 64 KiB chunks encrypted with
XChaCha20-Poly1305 in the STREAM construction: a chunk can't be moved, dropped or repeated, the
last one is marked so a file cut short is refused, and the whole header is the associated data
of every chunk so it can't be edited. A wrong key and a damaged file look the same to the
cipher, and are said that way.

The key is also in the backups' own `secrets.toml`, which is no help on a new machine: **the
page shows it once with a way to save it, and says to keep a copy elsewhere.** A new key is
used for new backups; old ones keep the key they were encrypted with.

## 3. Reading one

The archive is read in this process (the `tar` and `flate2` crates). Every member is written by
us, into a directory we made, after its name is checked:

- Only regular files and directories. A symlink, hard link, device or anything else is refused,
  so no link is ever created on disk.
- Only under `manifest.json`, `config/` or `data/`; no absolute name, no `..`.
- A name that appears twice is refused.
- What comes out is counted against the room there is (nine tenths of the free space, at most
  64 GiB), and against a member that lied about its size, so a small file can't fill a disk.

## 4. Settings: `backup.toml`

Its own file in the config directory, written whole by Irori, so the page can change it without
being able to touch `irori.toml` (where `allow_unauthenticated_lan` lives). Passwords, the key and
cloud secrets are in `secrets.toml`, under `[backup]`. Read each minute: a change applies at once.

```toml
enabled = true          # back up on the schedule; making one by hand works either way
encrypt = true          # off is refused while there is a destination
at = "03:00"            # in the home's time zone
days = []               # mon … sun; empty is every day
keep = 7                # newest kept, here and at each place
keep_days = 30          # and none older than this (the newest is always kept)
include_history = true
before_update = true

[[destination]]
kind = "path"           # a folder: a mounted share (SMB, NFS), a USB disk
name = "nas"
path = "/mnt/nas/irori"

[[destination]]
kind = "webdav"         # Nextcloud, ownCloud, most NAS boxes; password in secrets
name = "cloud"
url = "https://cloud.example.com/remote.php/dav/files/me/irori"
username = "me"

[[destination]]
kind = "s3"             # AWS, Backblaze B2, Cloudflare R2, MinIO; secret key in secrets
name = "b2"
endpoint = "https://s3.us-west-000.backblazeb2.com"
bucket = "my-backups"
region = "us-west-000"
prefix = "home"
access_key = "…"
```

The runner wakes every minute and asks one question: has the latest scheduled time passed
with no backup since? That is what makes a machine that was off at 03:00 back up when it
comes on, a changed schedule apply at once, and a failure try again (but not more than every
fifteen minutes). How it went is in `<data>/backup-status.json`.

## 5. Backup locations

After a backup is made it is copied to every destination, then each is kept to `keep` and
`keep_days` by the time in the file's name. **Only encrypted backups are copied**, and settings
that would send an unencrypted one off the machine are refused. Retention at a place deletes only
names this module writes (`is_backup_name`): a folder with other things in it loses none of
them. A place that can't be reached is said in the status and tried again with the next backup;
it doesn't undo the one made here. WebDAV is PUT, MOVE, PROPFIND, DELETE and GET (the folder is made
if it isn't there); a file is sent as `.name.part` and moved into place, so a dropped connection
never leaves a short backup under a real name. S3 is path-style, signed with Signature Version 4.
A password is only sent over `https://`, or over `http://` to the home's own network (a private
address, `localhost`, a name with no dot or ending `.local`/`.lan`). Small requests give up after a
minute; a transfer is given time for a link of 256 KiB/s.

## 6. Restore

A restore is **staged**, then **applied the next start, before the config or the database is
opened**: nothing replaces files a running server has open.

1. The backup is opened (encrypted ones with the key typed, or, if none is typed, the one this
   Irori has on file) into `<data>/restore-pending/`, checked (§3; format and version; the
   database passes `PRAGMA integrity_check`) and a plan is written.
2. The page asks Irori to restart (`irori backup restore` just goes on).
3. At start the plan is applied. First the home as it is is kept as `pre-restore-…`; if that
   fails nothing is replaced. Then the parts chosen are put back, and what happened is written
   to `restore-result.json`, which the page shows until dismissed. If putting the parts back fails
   half way, the home is put back from the safety copy and the result says so; the plan also
   warns, before anything is staged, when the backup's `irori.toml` has `allow_unauthenticated_lan`
   on, since restoring settings brings it back.

**Parts.** A part left out stays as it is now.

| Part | What |
|---|---|
| settings | `config/` (replaced whole; dot-entries stay) and `data/tls/` |
| extensions | what extensions remember: `data/extension-data/` and any other state beside the database |
| state | `irori.db` except history: sign-ins, tokens, the assistant's chats, what extensions keep |
| history | the history tables |

"Everything but history" builds the database from the backup's and copies the current history
tables into it. A config that doesn't load is what the home was: it is said as a note, not a
refusal. A backup from a newer Irori is refused (a `0.0.0` development build reads anything).

**After.** If the restore replaced who is signed in, the person signs in again with the
backup's people and passwords. The official extensions the backup's home had installed are
fetched again, except any that need full access to the machine, which wait for a person to
approve them from the Extensions page.

**A home with no owner yet** can be restored instead of set up: `/api/setup/backup/upload`,
`…/verify` and `…/restore` are open exactly as setting up is, and refuse once there is an owner.

**Offline.** `irori backup restore` stages and applies in one go, for a dead or new machine. It
refuses when a server answers at `<data>/cli.url`.

## 7. Surfaces

- Page: Settings → Backups (an owner's): the list with Download, Restore… (parts, key, Verify) and Delete; Create backup; upload a file; the schedule; the key (shown on request, with
  a file to save); backup locations (add, Test, Browse and copy a backup to this machine, Remove); what the
  last restore did. The welcome offers to restore.
- CLI (`docs/specs/cli.md`): `irori backup create|list|verify|restore|key`. They work on files.
- API: `GET/POST /api/backups`, `GET/DELETE /api/backups/{name}`, `POST …/{name}/verify`,
  `POST …/{name}/restore`, `POST /api/backups/upload`, `PUT /api/backups/settings`,
  `GET/POST /api/backups/key`, `GET /api/backups/destinations/{place}` (and `/test`, `/fetch`),
  `POST /api/backups/restore/cancel|seen`, and the three `/api/setup/backup/…` routes.
- Irori's own device: `sensor.irori_backup_status` (`ok`, `failed`, `none`) and
  `sensor.irori_backup_age` (hours since the last good one), so an automation can say when
  backups have stopped.

## 8. Who may

A backup holds password hashes and secrets. The routes are not in the auth table's read list, so
only the owner's session with the page's `x-irori-ui: 1` header gets in, and a token never does
(`docs/specs/api.md` §1). The one exception is §6's setup routes.
