# Spec: the public API

Status: **shipped** (ROADMAP C16, C17).

This is the door for a program that is not Irori's own page. The page signs in with a
session cookie (`docs/specs/config.md` §3.10). A script, a wall tablet, the CLI, or an
extension that dials in uses a token.

Home Assistant's API is the reference for the shape: an HTTP API plus a websocket, and a
long-lived token sent as `Authorization: Bearer`. Irori keeps the parts that make a
program easy to write and leaves the rest out.

- Commands are HTTP. The websocket only pushes changes. There is no second copy of every
  call on the socket.
- A token carries scopes. It is not a copy of its owner.
- A token cannot mint tokens, add people, install extensions, or restart Irori. That stays
  on the page, with the owner's session.
- There is one token, shown once. There is no refresh-token exchange.

---

## 1. Who is calling

| Caller | Credential | May |
|---|---|---|
| The page | Session cookie, and `x-irori-ui: 1` on every change | Whatever that person's role allows (C14) |
| A program | `Authorization: Bearer <secret>` | The scopes on its token |
| An extension that dials in | A token made for that extension, on `/api/extension` | Speak the extension protocol (§6) |

A request that sends a bearer token is that token, even if a cookie is on it too. The page
does not send a bearer token.

A bearer token does not need `x-irori-ui`. That header exists so a website cannot use the
browser's cookie. A token is a secret the program was given.

The API is not open to other websites. There is no CORS header on `/api/…`. A program calls
Irori directly. A page on another origin cannot.

Errors are `{ "error": "…" }` with an HTTP status. The sentence says what was wrong.

## 2. Tokens

An owner creates a token on the page (`POST /api/tokens`). The response carries the secret
once. Irori keeps the SHA-256 of it, the same way it keeps a session, and cannot show it
again.

```json
{ "name": "Kitchen tablet", "scopes": ["states:read", "services:call"] }
```

`name` is what the person types. The id is a slug of it (`kitchen_tablet`). Two tokens
cannot share an id.

A token belongs to the person who created it. It works while that person still has a
password, or while the home has no passwords yet. Removing the person, or setting the home
up again through the welcome, revokes their tokens. Changing a password does not: a token
is its own secret, and is revoked by deleting it.

The secret is `irori_` and 64 hex characters from 32 random bytes. A home holds at most 32
tokens.

`GET /api/tokens` lists them without secrets. `DELETE /api/tokens/<id>` revokes one. Both
are for an owner on the page. A token cannot call them.

### 2.1 Scopes

The same names an extension declares (`docs/specs/extensions.md`). A token lists the ones
it needs and no others.

| Scope | Allows |
|---|---|
| `registry:read` | `GET /api/devices`, `/api/entities`, `/api/areas`, `/api/floors`, `/api/floorplan`, `/api/extensions`, `/api/apps` |
| `states:read` | `GET /api/states` |
| `registry:read` and `states:read` | `GET /api/home` (the whole picture) |
| `history:read` | `GET /api/history/<entity_id>` |
| `services:call` | `POST /api/command` |
| `events:read`, with the two reads above | `GET /api/ws` |

Anything else — users, extensions, the floorplan's edits, restart, the assistant, tokens —
answers `403` to a token. The message names the limit: `this token doesn't include
states:read`, or `a token can't change how the home is set up`.

A service call made with a token is recorded as `origin.type = "api"` and that token's id
(`docs/specs/entities.md` §6). A call from the page stays `user`.

## 3. HTTP

`/api/dev/…` is gone. The same paths live at `/api/…`, with the same methods and bodies.
`/api/health`, `/api/session`, and `/api/setup` were already there.

`GET /api/home` is the picture the page shows: devices, entities, states, extensions,
areas, floors, held devices, and the floorplan when one has been drawn.

`POST /api/command` is `{ "entity_id", "command", "data"? }`. `command` is one of the
entity's actions (`turn_on`) or `toggle`. The response is the entity's state once the
protocol has confirmed, or an error.

## 4. The websocket

`GET /api/ws` upgrades. The cookie or the bearer token is checked on the upgrade, the same
way as any other request. A rejected upgrade is a normal HTTP error. There is no
auth-required round trip after the socket opens, and the client sends nothing.

Each text frame is one JSON object.

The first frame is the current picture:

```json
{ "type": "snapshot", "home": { } }
```

`home` is the body of `GET /api/home`.

After that, Irori pushes:

| Frame | When |
|---|---|
| `{ "type": "state", "entity_id", "state" }` | One entity's state changed. `state` is an `EntityState`. |
| `{ "type": "changed" }` | Anything else: a device or entity appeared or was renamed, an extension's status, the place, or settings (areas, floors, the plan). Read `GET /api/home` again. |

A client that falls behind is sent a fresh `snapshot` instead of a gap. When the socket
closes, the client connects again and gets a snapshot.

The page uses this and does not poll the home. It still asks `/api/health` on a slow
cadence, because uptime and the boot id are not home events.

## 5. What the page tells an extension's page

The shell posts `{ "irori": 1, "event": "changed" }` to an extension's frame when the
picture changes (`docs/specs/automations.md` §B4). The frame reads the shell's current
registry and states through the bridge. It does not call `/api/…` itself.

## 6. An extension that dials in

A package Irori starts itself still speaks newline JSON on stdin and stdout
(`docs/specs/protocols.md` §10). An extension that runs somewhere else dials in.

Its manifest says `inbound = true` and has no `run` command. Irori does not start a
process. It waits, and the Extensions page says so.

```toml
[extension]
id = "zwave"
name = "Z-Wave"
version = "0.1.0"
irori = ">=0.1.0"
inbound = true

[[contributes.protocol]]
iot_class = "local_push"
entity_kinds = ["switch", "sensor"]
```

The owner creates a token for it, after the package is installed:

```json
{ "name": "Z-Wave", "extension": "zwave" }
```

No scopes. This token cannot call the HTTP API. It only opens `GET /api/extension`.

Each text frame is one JSON message, the same `FromExt` and `ToExt` a child process
writes as a line. Irori sends `hello` first, with the extension's settings, then the
service calls and the rest. The extension sends `describe_device`, `state_report`, and
the other messages a protocol already sends. One connection at a time. A second is
refused while the first is open. When it drops, Irori waits for it to connect again.

The extension's declared API scopes are still enforced on the engine operations it sends
(`docs/specs/automations.md` §B2). The token is who it is, not an extra set of scopes.

An inbound extension has no stderr pipe, so its log window stays empty. What it's doing
comes from the protocol messages it sends, and from the status Irori shows while it waits.
There is no log message on this socket.

## 7. Not in this spec

| Topic | Where |
|---|---|
| Per-area permissions, an audit log | C28 |
| The CLI's `token` command | C22 |
| Signing extension packages | C26 |
| CORS, refresh tokens, a command channel on the websocket | Left out on purpose (§1) |
