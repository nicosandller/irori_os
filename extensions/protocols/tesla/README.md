# irori-protocol-tesla

Tesla cars and Powerwall or solar sites, through Tesla's Fleet API, with what the API costs kept
in view. Tesla bills every call to the developer account (reads, commands and wakes, at
different prices) against a monthly credit; this extension counts what it spends, paces its
reading so the month ends inside an allowance you set, and shows where the money went on its
own page, **Tesla usage**.

Commands to cars are signed in Rust with Tesla's vehicle command protocol, so there's no proxy
or helper program to run. Readings and commands all go through Tesla's servers: nothing talks to
the car or the Powerwall on the home network.

## Setting it up

The extension signs in as a developer app on [developer.tesla.com](https://developer.tesla.com).
If Home Assistant's Tesla Fleet integration is set up, reuse everything it has: the app, its
domain and `tesla_fleet.key`. The car already has that key, so nothing has to be paired again.

```toml
[tesla]
client_id = "…"
client_secret = "…"          # a secret: kept in secrets.toml, never shown again
domain = "tesla.example.com" # the app's Allowed Origin, where Tesla finds the public key
private_key = "…"            # tesla_fleet.key from Home Assistant; blank to make a new one
# redirect_uri = "https://tesla.example.com/irori/tesla"   # Advanced; must be an Allowed Redirect URI
```

Then the integration's card walks through what's left, one step at a time:

1. **The public key on the website.** With a new key, the card shows it to copy to
   `https://{domain}/.well-known/appspecific/com.tesla.3p.public-key.pem`. **Check the key
   again** registers with Tesla and compares.
2. **Registering** the app with Tesla, in North America and Europe. Done on its own.
3. **Signing in.** The card links to Tesla's sign-in; afterwards, paste the address you land on
   into **Finish signing in**. The page there doesn't have to exist.
4. **The key on each car.** For a car without Irori's key, the card links to
   `tesla.com/_ak/{domain}`: open it on the phone with the Tesla app, approve, then tap the key
   card on the console. Until then the car is read but not commanded.

Setup never waits on a car: a sleeping car is listed, shown as asleep, and read when it wakes.

## Reading

- **Which cars are awake** is checked with one call for all of them. A car is read when it
  wakes, then as often as the plan allows while it's charging, driving or warming up, and
  otherwise every few hours, so it can fall back asleep.
- **A sleeping car is never read, and never woken to be read.** A read would be billed and
  answer "asleep"; a wake costs ten times a read and drains the battery. What it said last stays
  on show. Home Assistant shows trunks as open while asleep (#136011); Irori keeps the last
  value.
- **What a car said last is kept** between runs, so the entities have values at start (Home
  Assistant left them unknown until the car woke, #146030).
- **Energy sites**: power flows on the plan's gap, the day's totals every few of those, settings when the account is listed. A site with no battery and no
  solar (a Wall Connector alone) is skipped.
- **One car Tesla refuses** (a car that doesn't allow it, say) gets its own line in the health,
  and the others carry on (#189753 took Home Assistant's whole setup down).
- **Refresh** on a car reads it now, and its name says what that costs ("Refresh (≈$0.002)").
  On a sleeping car it shows what Tesla last heard instead, and wakes nothing.
- **Wake** wakes it (≈$0.02), then reads it once it's up.

### Polling

A select on the **Tesla account** device, and on the page:

| Mode | Reads |
|---|---|
| Off | Nothing on its own: only Refresh and commands |
| Saving | A car only while charging or driving, every 10 minutes |
| Balanced (default) | As often as the allowance allows, from every 2 minutes |
| Responsive | From every minute while a car is doing something |

Every mode stays inside the allowance: the gaps stretch as the month's money runs low, to at most
6 hours. With nothing left for reading, cars and sites are read only on Refresh; commands still go
out.

## Commands

**Cars:** lock, climate (on and off, temperature, and keep, dog and camp modes), vent or close
the windows, trunk, frunk, charge port, start and stop charging, charge limit and current,
defrost, sentry mode, steering wheel and seat heaters, honk and flash.
**Energy sites:** backup reserve, operation mode, storm watch.

- **A sleeping car is woken once** ($0.02), and the command tried again every few seconds until
  it lands, which costs a command's price a try. Every command waiting for the same car shares
  the wake, and a car slow to wake isn't woken again while the command is still trying. A command may take up to 80 seconds this way, inside the 90 the core waits.
- **A command is sent again only when the car surely didn't act on it**: it was asleep, or turned
  the message away unread. Honk and flash are never sent twice. When Tesla doesn't answer, the
  error says the command may or may not have reached the car.
- **No read follows a command.** The entity shows the new state at once, credited to the call
  that asked, and the next read puts it right if the car disagrees.
- **The trunk opens with a toggle** (as in Tesla's own library), so it's sent only to a trunk
  known to be shut by a reading from the last two minutes. Otherwise the car is read first
  (≈$0.002, once it's awake for the command anyway): Home Assistant, the Tesla app or a hand may
  have opened the trunk since. When Tesla doesn't answer a trunk command, Irori forgets whether
  the trunk is open, so the next one looks first.

Commands are signed (HMAC, not encrypted: Tesla's servers read them to check them against the
sign-in). Irori keeps a session with each of the car's two computers. The car's key is trusted
the first time it's seen and held to while Irori runs. The signing follows
[teslamotors/vehicle-command](https://github.com/teslamotors/vehicle-command) (Apache-2.0); its
protobuf messages are written out by hand in `src/signed/proto.rs`.

## What it costs, and the usage page

Tesla bills **every answer below 500, including "asleep" (408) and "slow down" (429)**, and has
no API that says what an account has spent. So the extension keeps its own meter: every call it
makes, by car or site, by kind (read, command, wake) and by why (the schedule, Refresh, a command
and who asked for it, a wake for a command, setup). Before a call leaves it waits for room under
Tesla's per-car limits (60 reads, 30 commands and 3 wakes a minute), so it never pays to be told
to slow down.

The **Tesla account** device shows the month: Irori's spend, the whole account's (once the
dashboard figure is in), the projected end of month, and what's left of the allowance. Its
controls are the polling mode and the **monthly allowance**. Like any device's controls, anyone
who may control the device can change them: an owner, and a regular user who has its room, or the
whole home if it's in none. Put the device in a room only the owners have to keep them to owners.

**Tesla usage**, the extension's page, shows the month against the allowance and when it runs out
at this rate, each day's calls by kind for this month and last, which car or site spent what,
the recent billed calls with why each was made (an asleep or rate-limited answer is flagged as
billed), and the controls. On the page only an owner can change them.

Prices default to Tesla's in US dollars; the settings' **Billing** group takes another currency
and Tesla's rates for it. Which endpoints are billed as what follows Tesla's docs where they
could be read; the rest are counted as reads, so the meter errs high.

### Sharing the credit with Home Assistant

Every app on the developer account spends from the same monthly credit, and Tesla tells no app
what the others spent. Now and then, read **Spent so far this month** off Tesla's developer
dashboard and enter it on the page (or **Update from Tesla's dashboard** on the integration):
Irori takes what's beyond its own count as the other apps', works out what they spend a day,
and leaves room for them to the month's end. The page says how long ago the figure was entered.

The allowance is for the whole account, Home Assistant included. Commands share Home Assistant's
key too: when Home Assistant's commands move the car's counter, the car turns Irori's next one
away with a fresh session, which Irori checks and takes, and sends the command again.

When Tesla suspends the API because the billing limit is reached, the health says so, where to
raise the limit, and when readings resume: it's never shown as a sign-in problem (Home
Assistant's #146212).

## Not yet

- Streaming (Fleet Telemetry), so an awake car's changes arrive without reads.
- Cars that take only Tesla's older, unsigned commands (Model S and X from before 2021) are read
  but not commanded.
- The Wall Connector.
