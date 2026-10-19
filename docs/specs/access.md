# Spec: who may do what (people, rooms, activity, their own automations)

Status: **shipped** (ROADMAP C28). Roles and sign-in are C14 (`config.md` §3.10), tokens for
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
- The page, the CLI and the API refuse a room that isn't there. In a hand-edited file an id that
  is not an area is a warning in the log, once, and a person with one fewer room, never a
  rejection: areas and users are two files (`config.md` §6).
- **Removing an area from the page, the CLI or the API takes it off everyone's list first**, and
  the log records it. A room made again under the same name gets the same id (`entities.md`
  §4.2), so without this the old access would silently come back. If the second step fails,
  somebody has one room fewer than they should, never one more. A room deleted by editing
  `areas.toml` has to be taken out of `users.toml` by hand as well.
- A person whose last room is removed has no rooms (`areas = []`), not the whole home.
- An owner has none: making somebody an owner gives the whole home back, and the list is gone.
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
| The home, entities, devices, states, areas, floors, floor plan | Only what is in their rooms. A floor with none of their rooms is gone. On the plan, the shapes of their rooms and their devices stay, and so do the walls of a floor they are on: a wall is structure, not a thing in the home. |
| A device they can see | Without the hub it is reached through, when that is not theirs, and without the room it asked for by name (`suggested_area`). |
| The live feed (`/api/ws`) | A snapshot and changes for those only. A change to the home's shape is a nudge to read it again, which gives them only theirs. Changing a person's rooms or role ends their open feeds; the page reconnects to the new view. A hand edit of `users.toml` does the same within a few seconds. |
| History, a camera picture | Of an entity in their rooms; anything else is 404. |
| A command | Only to an entity in their rooms (§2.1). |
| Extensions, devices waiting to be added, updates waiting to install, extension pages | None. What the extensions are doing and what they have found is the owner's. |
| Other people | A person is told about themselves only. |
| The assistant | Asked everywhere they can ask it (the sidebar mark, the Ask buttons on a device, an automation and the floor plan), and **answered from their rooms** (§2.4). |
| Where the home is, what can be installed, the machine, how history is kept | 403: these describe the home as a whole, not a thing in a room (§2.3). |
| Setup: the logs, tokens, backups, anything that writes | The owner's, as before (§2.2). |

A thing the person cannot see answers **404**, not 403, so an id cannot be tried to find out
whether it exists, and with the very words a thing that was never there gets: the answer for a
camera they have no room for is the one for a camera with that name that does not exist. This holds
for a command too, and **before** anything is said about who may send it, or "only an owner may
install an update" would confirm that the update exists. 403 stays for things that exist for them
and are not theirs to change (setup), and for the addresses of §2.3, which are not secret.

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

### 2.3 Addresses that are for the whole home

`GET /api/place`, `/api/catalog`, `/api/system`, `/api/system/usage`, `/api/serial-ports`,
and `/api/recorder` answer **403** to a person who has only some rooms and as before to
everybody else. They are addresses, not things: nothing in them can be asked
about by id. One function in `server/auth.rs` (`whole_home_only`) lists them, and the table that
says who may ask for what has a case for each.

One address is not closed, on purpose for now: the Automations page asks its engine what a time
trigger will do (`clock.next`, `clock.holds`), and the answer carries the home's time zone and
today's sun times, from which a careful person could work out roughly where the home is. A person
with only some rooms is told them too. Closing it means the page previewing a time without them;
it is left for a follow-up and listed in the pull request.

### 2.4 The assistant

A person with only some rooms may ask the assistant anywhere a regular user may, and what it
knows is what they have. One `Reach` rides on the turn, and everything the model is given or finds
goes through it, in one place each:

- **The picture it starts from** (devices, rooms, floors, the floor plan, what each entity is
  reporting) is the list of §2 for them. A device, an entity or a floor outside their rooms is
  not in it, and a tool asked for one answers as it would for one that was never there ("there's
  no device …", "no changes in the last 24 hours").
- **Automations** are the ones the Automations engine shows them: their own (§4), checked against
  what they reach. The model suggests a flow the same way, and a suggestion is only a draft on the
  canvas: saving it is held to §4.
- **Not offered, and refused if asked for anyway**, to anybody who is not an owner (a regular
  user with the whole home included): the logs (they hold the sign-in lines), Settings, the helper
  builder and its tools, and drawing on the floor plan or adding floors and rooms. The
  conversations about Settings and the helper builder are the owner's; to anybody else they don't
  exist.
- **What is said is theirs.** Owners share their chats. Anybody else, a regular user with the
  whole home included, has chats of their own: what the model says to an owner is drawn from
  Settings and the logs, and to a person with rooms from those rooms, so neither is for anybody
  else to read, clear or stop. A person's chats are forgotten when anything they were told in
  them stops being theirs: their rooms edited, a room deleted, a device moved out of it, the
  whole home narrowed to some of it, or the person removed. It is worked out from what they could
  reach when they last asked, so it holds whichever way the change came, a hand edit of
  `users.toml` included. Gaining a room loses nothing. (A person removed by hand-editing the file
  leaves their chats behind until the same name is used again; whoever has that name gets them
  only if they reach all of what they say.)
- **How it is set up is the owner's.** Anybody else is told whether a model is ready and whether
  what they ask leaves the house; not which model, where it is, the key, or the standing
  instructions. Those, and installing or changing a model, stay behind setup (§2.2).

What the model is told is not a way round anything: it reads only the lists above, and the leak
test asks it, as a person with a room, for a device in the cellar by every route it has.

### 2.5 Nothing is let through by being forgotten

Every address under `/api/` is listed, with what a person who has only some rooms gets from it, in
a table in the server's tests. The test reads the router's source for its addresses, so one added
without a row fails the build. It then asks each address as such a person, with the ids and
the words of everything outside their rooms known, and fails if any of them comes back. The
list the page is built from is taken apart field by field in the code, so a field added to it
does not compile until someone has said what a person with rooms is shown of it.

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
| A backup made, uploaded, deleted, fetched; the recovery key looked at or changed; a backup downloaded (the two reads that take something out of the house); a restore asked for, and, after the restart, applied | |
| A flow saved, turned on or off, deleted, brought back to an older version, or tested for real, by a person (a dry run only imagines, and is not written); a flow turned off because of a change in rooms; a removed person's flow handed over | |

Every request that changes something is recorded at the one place all requests pass through
(the guard), with who, the address and the answer; the part that knows more (the entity a command
was for, which command, what was changed about a person) adds its words. A table in
`crates/irori/src/server/trail.rs` names the row each address leaves, and a test fails when an
address is added without a row; one that slips through anyway is still recorded, as `change`.
Details are words, never what was typed: the **name of a command, not its data** (a lock's code
is data), and "password" in a list of what changed, not the password.

A refusal is a row with the reason in it ("not in their rooms", "only an owner can change how the
home is set up"). The person is told what a thing that is out of their rooms always says, that
it is not there (§2); the owner reading the log is told what it was.

A person who is **nobody** (a wrong password, a request with no sign-in) leaves a row only for
signing in and for setting the home up, and at most 30 of them a minute: anyone who can reach Irori
can make those, and a log that wrote them all would be a way to push out what matters. The ones
over are counted and said once (`tries_unwritten`). A wrong password for a name that is somebody
names them; a name that is nobody's is not written at all, because it may be a password typed in
the wrong box.

**Who** is kept as the id and the name at the time. An id can be made again after a person is
removed (`config.md` §3.10), and a token is gone from its table when it is revoked; the row
keeps the name of the person, the token and the person it belonged to, so it still reads.

A command row keeps the id of the call's context. The state change that follows carries that id as
its `parent_id`, which is how an entity's history can say "turned on by Leo" without the recorder
storing a person on every point. That needs the log to be kept at least as long as history, so
`retain_days` is never less than the recorder's. `GET /api/history/<entity_id>` carries it as
`by`, a map from a call's context to a name. Whoever has the whole home is told who. A person with
some rooms is told their own name, and "someone" for the rest. Only a command a person or a program
sent is there: a switch pressed on the wall, or a flow, is not.

### 3.2 Keeping it

`[audit]` in `irori.toml`:

| Key | Default | |
|---|---|---|
| `retain_days` | 90 | Older rows are removed. Never less than `[recorder] retain_days`. |
| `max_rows` | 100000 | Oldest first. A program that calls the API every few seconds is recorded by design, and this is what it can cost. Cutting goes down to 90% so it does not happen at the next row; the log says so (`activity_trimmed`) and names the actor that wrote more than half of what is left. |

Rows are written by a writer of their own that batches. Sign-ins and sign-outs, changes to
people and to tokens, a restart, the recovery key and a restore are written **before the answer goes
back**, because a crash or a restart is what follows them. A row that cannot be written because
the queue is full is counted and said in the log (`activity_lost`), so a gap is never silent.

**A backup holds it as its own part, "activity", and a restore leaves the log as it is** unless
that part is chosen (`backup.md` §6). A restore is itself written to the log: once when it is asked
for (`restore_asked`), and again by Irori when it has been applied, in the log as it then stands
(`restore_applied`), so a restore that brings the backup's log back still leaves the line that
says it did. The record of a restore that rolled itself back would be worthless.

### 3.3 Reading it

Owner only, from a session: Settings → **Activity**, `irori audit list` (`--user`, `--kind`,
`--outcome`, `--since`, `--json`), and `GET /api/audit`. A token never reads it (`api.md` §2.1).

It is evidence against mistakes and against other people. It is not tamper-proof against the
owner: the owner holds the machine, and Irori does not pretend to be a witness against them.

## 4. A regular user's own automations

A flow gets an optional `owner` (a user id; absent means the home's own, as every flow so far).
It is not part of the flow's version (`flows.md` F6), so naming an owner forks nothing. A saved
version records who saved it, and a test or dry run records who started it.

- **A person sees and edits their own flows.** An owner sees all of them, with whose each is. Another person's flow, and the home's own, answer a regular user as a flow that is not there (to read, to change, to test, to delete); saving over another's id says the id is taken, which tells them a flow of that name exists and nothing else of it.
- **Whose a flow is, is not the page's to say.** A flow a regular user saves for the first time is theirs. One that is there keeps its owner whoever saves it: an owner who edits a person's flow leaves it theirs, and the owner's own flows stay the home's own (so removing or demoting an owner does not turn off the household's automations). Whatever `owner` a draft, a suggestion or an old version says is replaced.
- **What a flow reads is its owner's too.** A draft, a dry run, a backtest and the timeline are told the states and history of what its owner can see and nothing else, however the draft is written.
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
  fails is **turned off and says why**, and the log records it once, when it stops (not on every
  later check). The reason is the same one a flow that names an entity that does not exist gets,
  worded for a person's rooms ("`light.kitchen` = no such entity in your rooms"): it does not
  say that the thing exists, or what it is called, so that being out of their rooms and not being
  there stay one thing. Nothing is deleted.
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
only, so there is nothing yet for an owner to approve. The shell tells the extension who is asking with each request
(`caller`), and the page cannot set it. A regular user's question goes only to an extension that has
the flag and is trusted with it; to any other the page answers as if it had none. Only such an
extension may call `get_reach` or ask the core to write a remark in the log (`note`).

## 5. Tokens

Unchanged: a token carries scopes and is not a copy of its owner (`api.md` §1). It is whole-home
within its scopes. A token cannot read the activity log. Rooms on a token are a follow-up.
When an owner is made a regular user, their tokens are revoked (they may no longer make them).

## 6. Not here

A view-only role; a tier between owner and user; guests that expire; rooms on tokens; presence- or
time-based access; per-person dashboards (when dashboards arrive, X12, they take their entities from
§1.2); local-only people; two-step sign-in; any check on a call a running flow makes.
