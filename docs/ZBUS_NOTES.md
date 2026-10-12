# ZBUS_NOTES.md — zbus 5 pitfalls found the hard way (READ FIRST)

Per Amendment 4 lesson (a). Read this before writing ANY new D-Bus module.
Every item below cost a real debugging session; the fix pattern is linked to
live code in this tree. zbus version: 5.x (see Cargo.lock; API drift from 3/4
blog posts is the norm — trust the vendored source, not blog posts).

## 1. `a{sv}` dict values arrive WRAPPED in a variant

The wire form of `a{sv}` is a dict whose values are *themselves* variants.
zbus gives you `Value::Value(Box<Value>)` for each entry — NOT the inner
type. If you match on `Value::Str`/`Value::U32` directly on the dict value,
every lookup silently fails (returns None / default).

Fix: unwrap one layer while copying into your map — see `dict_map` in
crates/lme-shell/src/tray.rs (the `Value::Value(inner)` arm). This was a real
wire-form bug: every dbusmenu property would have parsed as empty before it.

Symptom: all properties "empty" despite the bus dump showing values.

## 2. `Message::header()` returns the header BY VALUE (zbus 5)

zbus 3/4 examples do `msg.header().member()` on a borrowed header; zbus 5
returns an owned `Header` — take it as a value (`let h = msg.header();`),
then call `.member()`/`.sender()`/`.path()` on it. Compiler will force this
one; don't fight it with extra refs.

## 3. `receive_all_signals` filters on the PROXY'S OWN interface

"All signals" means "all MEMBERS of that proxy's interface", not "everything
at that destination". Consequence: `PropertiesChanged` (which lives on
interface `org.freedesktop.DBus.Properties`) NEVER arrives on a proxy built
for `org.kde.StatusNotifierItem` or `org.mpris.MediaPlayer2.Player`, even
though that's how every real app emits updates.

Fix (see D24 in DECISIONS.md, both listeners in tray.rs / mpris.rs):
subscribe PropertiesChanged on a SECOND proxy built with the
`org.freedesktop.DBus.Properties` interface at the same destination/path, as
its own listener (shares the alive flag with the primary). Alternatively use
an explicit `conn.add_match` rule or a raw `MessageStream` — but the second
proxy is the least code.

Symptom: register/unregister works (rides NameOwnerChanged) but property
updates never refresh the UI.

## 4. `emit_signal` takes the body by REFERENCE

`conn.emit_signal(dest, path, iface, member, &(arg1, arg2))` — the body is
`&Body`, so pass a reference to the tuple, not the tuple. Passing by value
gives a confusing type error.

## 5. `Structure::from` wraps `Value` fields in a variant

`Structure::from((Value::Str(..), Value::U32(..)))` produces fields each
wrapped in `Value::Value(..)` — zbus "helpfully" matches the signature
`(ov)` style. When READING such a structure, unwrap `Value::Value(inner)`
first (same as pitfall 1). When BUILDING one for a reply, either construct
with `Value::Value(Box::new(v))` explicitly (see the dbusmenu GetLayout reply
builder in tray.rs) or work in plain tuples and let serde handle it.

## 6. Misc zbus 5 API drift (each broke a build at least once)

- `Value::pixel()`-style getters return `Option`, not the value.
- Signal receive iterators: `.next().await` inside a tokio-less async
  block on the zbus executor thread; never `.block_on` from the UI thread.
- `zbus::blocking` wrappers exist for sync contexts; do not mix a blocking
  Connection with async proxies on the same connection.
- Name ownership: `conn.request_name` returns a `Result<RequestNameReply>`;
  check it — the e2e asserts on the reply, the daemon only logs.
- The zbus object server runs on its own executor threads (D4/D20 pattern):
  bridge to the daemon via channel → calloop source; the UI thread never
  awaits D-Bus futures.

## 7. Testing recipe (worked for tray + MPRIS + notifications)

Fake service as an example bin (examples/fake_sni, examples/fake_mpris),
spawned by the e2e against a PRIVATE session bus (`dbus-run-session` or a
manually spawned `dbus-daemon --session` with a socket in a temp dir). Assert
on: name ownership, golden UI after the signal, NameOwnerChanged-driven
removal. Keep a `service missing` e2e too (Amendment 4 lesson d): private
bus WITHOUT the service proves no panic + control hidden.

## 8. Rule of thumb

Before matching any `zvariant::Value` that came off the bus, ask: "could this
be a variant wrapper?" — if the signature contains `v`, unwrap first. Grep
`Value::Value` in this repo to see every place that lesson applied.

## 9. zbus 5.19 blocking API on shared connections + wakeup loss (D25 — READ BEFORE ANY NEW MODULE)

The MPRIS e2e exposed an intermittent (40–60%) wakeup loss inside
zbus 5.19/async-io when used from the full daemon: a Get goes out on the
wire, the peer replies, the socket drains — and the reply is never
dispatched to the blocked caller. Not the property cache, not the
executor driver exit, not the thread shape: even a canonical
single-thread `async_io::block_on` shape stalls in the daemon context,
while the identical standalone probe (examples/mpris_probe.rs) is 12/12
stable.

STATUS (Amendment 5): the "wakeup loss inside zbus" mechanism is a
HYPOTHESIS — it was never reproduced in a minimal probe (mpris_probe.rs is
12/12 stable); what is ESTABLISHED is the observable (wire reply never
dispatched in the full-daemon context) and the fix (self-healing below).

Rules distilled:

1. Prefer the ONE-THREAD/ONE-block_on shape for new D-Bus modules (see
   mpris.rs D25): connection, signal streams and calls as futures in a
   single async block, sources merged into one Stream. The daemon's
   still-blocking modules (notifications, tray) keep the D20 thread
   pattern — they serve object servers and have never flaked — but do
   NOT extend that pattern to new client-only modules.
2. EVERY push-driven feed needs a self-healing poll: a reconcile tick
   (ListNames diff or equivalent) + per-call timeouts raced against a
   timer. A lost wakeup then costs one tick, not a hang. This is the fix
   that actually made the flake disappear — everything else below was
   tried and did NOT change the stall rate (D25 ladder, steps 3-4).
3. (RETRACTED, Amendment 5) `CacheProperties::No` proxies: HYPOTHESIS,
   not reproduced in a minimal probe — the cache-deadlock theory did not
   fix the stall (ladder step 3, "stall identical"). The
   `dbus_util::no_cache_proxy` helper was prophylaxis and has been
   REMOVED; do not re-add it without a failing test that it fixes.
4. (RETRACTED, Amendment 5) the custom executor tick loop
   (`drive_executor_forever`): same — the executor-driver-exit theory did
   not fix the stall (ladder step 4, "stall identical"), never confirmed
   in a minimal probe, helper REMOVED.
5. Debug knobs: LME_SHELL_NO_NOTIF / LME_SHELL_NO_TRAY skip modules for
   bisecting; LME_MPRIS_DEBUG prints feed markers; mpris_probe.rs is the
   standalone pattern reference.
