# Spec: who may do what (people, rooms, activity, their own automations)

Status: **in progress** (ROADMAP C28). Roles and sign-in are C14 (`config.md` §3.10), tokens for
programs are C16/C17 (`api.md`); this spec adds what sits on top of them.

Two roles are enough for a household that trusts each other, and no more than that for a
household that doesn't: a child who should switch the lights in their own room, a guest who
should work the living room, a cleaner who should see nothing but the hall. This spec lets the
owner say which rooms a person has, makes that true on every address Irori answers, keeps a
record of who did what, and lets a regular user write automations that stay inside their rooms.

What Home Assistant has, and what is here instead:

| Home Assistant | Here |
|---|---|
| A policy engine since 2018: selectors by entity, device, area, domain; read / control / edit; groups merged with "true wins". Never finished (not every API checks). Custom groups only by editing a hidden file with Home Assistant stopped. | One selector, **rooms**, a list in `users.toml` the page also edits. Checked in one place per address, with a test that walks every address. |
| Hiding a card or a view, and kiosk modes. Search, direct addresses and camera streams still leak. | **A thing outside your rooms does not exist for you.** It is absent from every list and answers 404, as a thing that was never there would. |
| Every non-admin runs every automation and script; templates read any state, so a limit can't be checked. | A flow has an owner and names its entities as literals, so what it reaches is **checked when it is saved**, and again whenever the home or the person's rooms change. |
| The logbook shows who for changes made on the page. No record of sign-ins, settings, installs. | An **activity log**: who, what, when, and whether it was allowed, kept apart from history, and owner-only. |
| Presence, time windows, conditions (asked for, not built). | Not here. A rule that changes with the clock is not data a person can read in a file. |

## 1. Rooms

`users.toml` (`config.md` §3.10) gains one optional key:

```toml
[users.leo]
name = "Leo"
role = "user"
areas = ["leos_room", "hall"]     # absent: the whole home
```

- `areas` is a list of area ids. **Absent means the whole home**, as every `users.toml` written
  before this spec means. An empty list is a person with no rooms: they can sign in and see
  nothing. An owner has no `areas`; the key on an owner is rejected by the types and by the page.
- An id that is not an area is a warning in the log and a person who has one fewer room, never a
  rejection: areas and users are two files (`config.md` §6).
- **Deleting an area removes it from everyone's list in the same write**, and the log records
  it. A room made again under the same name gets the same id (`entities.md` §4.2), so without this
  the old access would silently come back.
- Renaming an area changes no id and no one's access.

### 1.1 What is in a room

A device is in the area the owner **saved** for it (`devices.toml`). A device that only says what
it would like to be called (`suggested_area`, `config.md` §5) is placed by name for the owner's
view, but **does not count** for a person's rooms: a device cannot join Leo's room by being
plugged in. An entity is in its device's room (`entity.area_id` is not set today, and when it is
it wins).

An entity with no device and no room (a helper, a computed value), a device with no saved room,
and Irori's own machine are **in no room**. A person with rooms does not see them and cannot use
them. A person without `areas` sees all of them.

### 1.2 The one answer

`irori-core` answers three questions about a person and no others:

- may they **see** this entity, device, area, floor?
- may they **send** this command to this entity?
- may they **write** this flow (§4)?

The answer for a person with rooms is a set of entities worked out when the registry, a device's
settings, the areas, or the person's `areas` changes, so asking is a lookup (ROADMAP C24's
latency budget applies). Nothing else decides who may see or control anything; the page, the
CLI, the assistant and the app bridge all ask it.

## 2. What a person with rooms gets

| | |
|---|---|
| The home, entities, devices, states, areas, floors, floor plan | Only what is in their rooms. A floor with none of their rooms is gone. |
| The live feed (`/api/ws`) | A snapshot and changes for those only. Changing a person's rooms ends their open feeds; the page reconnects to the new view. |
| History, a camera picture | Of an entity in their rooms; anything else is 404. |
| A command | Only to an entity in their rooms (§2.1). |
| The assistant | Reads and proposes within their rooms. A chat belongs to the person who started it. |
| Settings: the logs, other people | The logs are the owner's. A person is told about themselves only. |

A thing the person cannot see answers **404**, not 403, so an id cannot be tried to find out
whether it exists. 403 stays for things that exist for them and are not theirs to change (setup).

An open home (nobody has a password yet, `config.md` §3.10) has no one to limit; everybody is the
owner there, as before.

### 2.1 Commands only an owner sends

A regular user, whole home or not, may **not** send any command to an entity of kind `update`:
install, check, skip and clear skip. A token is never an owner, so it does not either (`api.md`:
"a token cannot install extensions"). An update changes the software and the firmware the home runs on;
it is setup (`config.md` §3.10: "nothing that changes how the home is set up"). This is one
function in `irori-types` that the address for commands and the check on a saved flow (§4) both
call, so the two cannot drift.

Lock and alarm commands carry their code with the call, as they always have; a person who may
send commands to the entity may send those.

### 2.2 Setup stays the owner's

Nothing in this spec lets a regular user change how the home is set up. Reading the logs (core,
each extension, the assistant's model), tokens and backups is closed to them too: the logs hold
sign-in lines and the names of everyone. Irori's own machine is a device in the home like any
other (C34); a person sees it when they see the rest (§1.1).

## 3. The activity log

A record of what people and programs **did**, kept in `irori.db` beside the sessions and
tokens, and not in the history tables. History is lossy by design (a queue that drops under load,
ten days kept, everything about a device forgotten when it is removed); a record of who changed
what must not be.

### 3.1 What is recorded

One row per event: when, **who** (a person, an access token, an extension, or Irori itself), what,
on what, **whether it was allowed** (`ok`, `denied`, `failed`), and a few words of detail.

| Recorded | Not recorded |
|---|---|
| Sign-in, wrong password, locked out, sign-out | Reading anything |
| A person added, removed, renamed, given another role, other rooms, a new password | The password, key, code or secret itself, ever |
| An access token made or revoked | Calls made by an automation (the trace has them) |
| A command sent by a person or a token, and one refused | State changes (history has them) |
| Areas, floors, devices, entities, the floor plan, settings, the place changed | |
| An extension installed, updated, uninstalled, its secrets set | |
| An update installed or skipped | |
| A backup made, deleted, restored | |
| A flow saved, turned on or off, deleted, by a person; a flow turned off because of a change in rooms | |

Every request that changes something is recorded at the one place all requests pass through, with
who, the address and the answer; the part that knows more (what was renamed to what) adds its words.
A new address that forgets to say anything still leaves a row.

**Who** is kept as the id and the name at the time. An id can be made again after a person is
removed (`config.md` §3.10), and a token is gone from its table when it is revoked; the row
keeps the name of the person, the token and the person it belonged to, so it still reads.

A command row keeps the id of the call's context. The state change that follows carries that id as
its `parent_id`, which is how an entity's history can say "turned on by Leo" without the recorder
storing a person on every point. That needs the log to be kept at least as long as history.

### 3.2 Keeping it

`[audit]` in `irori.toml`:

| Key | Default | |
|---|---|---|
| `retain_days` | 90 | Older rows are removed once a day. Never less than `[recorder] retain_days`. |
| `max_rows` | 100000 | Oldest first. A program that calls the API every few seconds is recorded by design, and this is what it can cost; the page says when rows went for size, and which token fills it. |

Rows are written by a writer of their own that batches, except sign-ins and changes to people,
which are written before the answer goes back.

**A backup holds it as its own part, "activity", and a restore leaves the log as it is** unless
that part is chosen (`backup.md` §6). A restore is itself written to the log afterwards. The
record of a restore that rolled itself back would be worthless.

### 3.3 Reading it

Owner only, from a session: Settings → **Activity**, `irori audit list` (`--user`, `--kind`,
`--outcome`, `--since`, `--json`), and `GET /api/audit`. A token never reads it (`api.md` §2.1).

It is evidence against mistakes and against other people. It is not tamper-proof against the
owner: the owner holds the machine, and Irori does not pretend to be a witness against them.

## 4. A regular user's own automations

A flow gets an optional `owner` (a user id; absent means the home's own, as every flow so far).
It is not part of the flow's version (`flows.md` F6), so naming an owner forks nothing. A saved
version records who saved it, and a test or dry run records who started it.

- **A person sees and edits their own flows.** An owner sees all of them, with whose each is.
- **What a flow may touch is the owner's rooms and the owner's commands.** The home checks of
  `flows.md` §4 (Layer 3) run against what the flow's owner can see, so an entity outside their
  rooms is "unknown", and against what they may send (§2.1), so a flow of theirs cannot install an
  update. Entities and variables in a flow are literals (`rules.md` §10.1), so this is complete:
  there is no run-time path to an entity the check did not see. Events that are not entities
  (`mqtt.message`) are the owner's only.
- **It is checked again whenever it could have changed.** A person's rooms change without a flow
  being saved: a device is moved out, an area is deleted, a device is removed, the owner edits
  `areas`, an owner is made a user. The engine already checks every flow whenever the registry
  changes; the core now sends that signal for any of these too, and the engine asks the core what
  each flow's owner can reach now (`get_reach`, engine API, `automations.md` §B2). A flow that
  fails is **turned off and says why** ("the Kitchen light is not in your rooms any more"), and
  the log records it. Nothing is deleted.
- **A person who is removed** leaves their flows behind, turned off, and the owner has them.
- **The engine never checks a call.** A running flow calls services as the engine, with the
  scopes it was given (`automations.md` §B2), exactly as before. This amends `rules.md` §19 and
  K15 in one respect: they said enforcement is where files are written; a person's reach can change
  with no file written, so the same *validation* also runs when the home changes. It is still not a
  permission check on a call.

### 4.1 Who may open the page

An extension page is the owner's unless its manifest says `users = true`, which means "regular users
may open this page and I limit what they can do". That is trust handed to the extension, so it is
shown in the install prompt like any permission (`extensions.md` §7). Until permissions are
approved one by one (ROADMAP C26), the flag is honoured for official packages (`official.toml`)
only. The shell tells the extension who is asking with each request (`caller`), and the page cannot
set it. Only an extension that has the flag may call `get_reach`.

## 5. Tokens

Unchanged: a token carries scopes and is not a copy of its owner (`api.md` §1). It is whole-home
within its scopes. A token cannot read the activity log. Rooms on a token are a follow-up.
When an owner is made a regular user, their tokens are revoked (they may no longer make them).

## 6. Not here

A view-only role; a tier between owner and user; guests that expire; rooms on tokens; presence- or
time-based access; per-person dashboards (when dashboards arrive, X12, they take their entities from
§1.2); local-only people; two-step sign-in; any check on a call a running flow makes.
