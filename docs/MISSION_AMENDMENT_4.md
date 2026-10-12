# Amendment 4: keep going (2026-10-13)

Read together with docs/MISSION_CONTINUATION.md and Amendments 2–3. Where they
conflict, this one wins. (No session-setup block was part of this amendment.)

You ended the last session at "Tier 1 complete" because of an estimate of the
remaining budget. That is not a stop condition. The mission is the whole shell,
and Tier 2 has not started.
- Stop only on a hard failure (a tool refuses, no disk you cannot free, context
  exhausted). Estimates are not failures. Every tier end already delivers, so
  continuing is safe.
- If you really must stop: commit, run scripts/gh-sync.sh and a lean checkpoint
  first. Then your last message starts with "STOPPED:", names the exact step you
  reached, and says what to resend. Never write "session complete" before FINAL
  has been delivered.

# Make resumption cheap (do first)

1. Save this message as docs/MISSION_AMENDMENT_4.md. If you still have the text
   of the continuation prompt and Amendments 2 and 3, save them as
   docs/MISSION_CONTINUATION.md, docs/MISSION_AMENDMENT_2.md and
   docs/MISSION_AMENDMENT_3.md, each without its "Session setup" block, unless
   they already exist. Credentials never go into any file.
2. Write docs/RESUME.md: the exact bootstrap steps for a fresh sandbox
   (credentials come from the owner, Recovery path b, read docs/MISSION_*.md in
   order, then STATE.md and NATIVE_PLAN.md, continue). Commit and run
   scripts/gh-sync.sh.

# Work order

1. Disk and delivery robustness (you hit a full disk twice and a tool timeout):
   - scripts/deliver.sh aborts early if free space is below 3 GB.
   - The replay uses one dedicated target dir and runs `cargo test --release
     --locked`, so only a release build exists. If the e2e harness only finds
     debug binaries, teach it to use the current profile's directory.
   - Delete the replay target dir right after the replay passes.
   - Split deliver.sh into resumable stages (export, replay, scan-and-zip, send)
     with a state file, each under about 5 minutes, so a timeout never forces a
     restart from zero.
2. Fractional scale (Tier 0.9), time-boxed to one work unit. sctk 0.21 has no
   helper, but you can bind wp_fractional_scale_manager_v1 and wp_viewporter
   yourself through the wayland-protocols crate with its staging feature. Verify
   the crate version and API in the vendored source first. Plan: buffer scale 1,
   viewport destination = logical size, buffer size = ceil(logical x scale),
   render at that scale, text and shadow caches keyed by scale. Add a
   fractional-scale global and a --scale flag to the mock compositor, and goldens
   at 1.0 and 1.25 (200 KB or smaller each). If one honest attempt blocks, fall
   back to the integer ceil scale with viewport downscale, record the blocker in
   docs/DECISIONS.md, and move on.
3. Tier 2 in the original order: control center shell and Controls tab, audio,
   brightness, NetworkManager, BlueZ, power profiles, idle inhibit, Notifications
   tab, clipboard history, calendar/to-do/timer/stopwatch/Agenda, weather,
   activity island, night light, capture, wallpaper, workspace board, charging
   helper client. Use the method that worked for tray and MPRIS: a fake D-Bus
   service in examples/, an e2e on a private bus, and a golden. Deliver TIER-2A
   after the control center, audio, brightness, network and Bluetooth, then TIER-2
   at the end. After that Tier 3, Tier 4 and FINAL as before.

# Lessons to make permanent

a. docs/ZBUS_NOTES.md: record the pitfalls you found and read it before any new
   D-Bus module: a{sv} entries arrive wrapped in variants; header params by value;
   receive_all_signals filters by the proxy's interface so PropertiesChanged never
   arrives (use an explicit match rule or message stream); emit_signal body by
   reference; Structure::from wraps Value fields in a variant.
b. Click ids: D23 was one range check swallowing another module's ids. Create one
   id-registry module with named, non-overlapping ranges (toasts, tray, panels,
   bar modules) and a unit test that fails on overlap or on a check outside its
   registered range. Use it for everything new.
c. Idle bar: every new periodic poll runs only while its widget is visible or
   enabled, and is listed in docs/PERF.md with its period. Re-measure idle ticks
   and wakeups at every tier end and put the numbers in the report.
d. Graceful degradation: every service module gets a "service missing" e2e (private
   bus without the service) proving no panic and that the control is hidden.
