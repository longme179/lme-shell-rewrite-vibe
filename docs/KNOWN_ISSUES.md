# KNOWN_ISSUES.md — live list (Amendment 5 debugging-cap format)

Every entry: symptom, repro, ruled out, not tested, workaround/status.
"root cause" / "upstream bug" only with a minimal repro; else "hypothesis".

## KI-1: golden e2e suite rare flake (~1/13 observed, 2026-10-12)

- **Symptom**: one golden-suite run of 21 tests reported 20 passed /
  1 FAILED; the immediately preceding and 12 following runs were green.
  The failing test's name and panic were not captured (summary-only grep).
- **Repro**: `cargo test -p lme-shell --test golden` repeatedly; observed
  once in 13 runs after the LME_E2E_SCALE env-var race was fixed.
- **Ruled out**: the fractional-scale env leak (LME_E2E_SCALE removed
  entirely — scale is now an explicit spawn_harness_full parameter);
  stale lme-shell/mock/fake processes (ps clean); tmp-dir growth.
- **Leading hypothesis (NOT a root cause)**: a daemon hit inotify_init
  EMFILE at startup under 21-way parallel e2e load and died, timing out
  the test's wait_log. 21 daemons x inotify instances vs the 128-instance
  user budget leaves little headroom with other suites running.
- **What was done**: (a) the EMFILE death class is FIXED — the daemon now
  survives inotify failure and falls back to a 5 s mtime poll (Amendment 5
  hygiene 4, daemon.rs insert_mtime_poll, e2e
  settings_poll_hot_reload_survives_without_inotify); (b) leaked
  dbus-daemons (82 found alive from `--fork` test buses) are fixed and
  killed, freeing instance budget; (c) the tier-end e2e x10 run
  (scripts/e2e-x10.sh) is the standing detector — if the flake recurs,
  capture the failing test name from /tmp/e2e-x*.log and escalate to a
  fix or a new entry.
- **Not tested**: deliberately exhausting inotify instances to force the
  EMFILE path end-to-end (judged not worth the flakiness it would add).

## KI-2: fractional-scale path validated only against the mock compositor

- **Symptom**: none (works). Recorded because Amendment 5 demands honesty
  about unvalidated rendering paths.
- **Status**: wp_fractional_scale_v1 + wp_viewporter are implemented,
  unit-tested and golden-validated at scale 1.25 against the mock
  compositor (buffer = ceil(logical x scale), viewport destination =
  logical, text/shadow caches keyed by scale; e2e fractional_scale_golden
  asserts the 1600x53 buffer + 1280x42 destination + the downscaled
  golden). The scale-1.0 path is byte-identical to TIER-1: no
  wp_viewport object is ever created unless a non-1.0 preferred_scale
  arrives (lazy engagement, wayland.rs).
- **Not tested**: a real compositor at 1.25 (niri). The mock's golden
  softness is the deterministic box-filter downscale in its capture path —
  a real compositor displays the physical buffer 1:1 (sharp).
- **Workaround/next step**: keep on the real-machine validation checklist
  (niri at 1.25, compare against docs/screens/bar.png). Do NOT re-enable
  any speculative path without that test. D26 in docs/DECISIONS.md.

## KI-3: RESOLVED 2026-10-13 — the control-center Flickable landed with Tier 2.6

- **Was**: the Controls tab clipped instead of scrolling on outputs
  < ~900 px. The weather card (Tier 2.6) made the overflow worse (content
  ~1000 px vs a ~600 px viewport), so the port was pulled forward from
  Tier 2.7.
- **Fix**: lme-ui `Flickable` widget (wheel scroll, translated layout,
  viewport clip per command, deliver re-layouts after the offset moves);
  ControlCenterPanel restructured into a fixed top (header + tabs) and a
  scrolling body; the daemon refresh now rebuilds IN PLACE and requests a
  re-layout, keeping the scroll offset across 5 s refreshes (tab switches
  reset it). Found while porting: `request_render` alone re-paints stale
  rects — in-place child swaps need `request_layout` (UiState gained it).
- **Proof**: unit tab_content_scrolls_and_rebuild_keeps_offset,
  widgets::tests::flickable_scrolls_and_clips; e2e
  control_volume_slider_flushes_action now scrolls to the slider.

## KI-4: tray panel list scroll may be inverted (hypothesis, not reproduced)

- **Symptom**: none observed (the tray panel in its e2e never has enough
  rows to need scrolling — max_scroll is 0, so the sign never matters).
- **Observation**: while landing the Flickable sign convention (dy > 0 =
  wheel down = offset advances; proven by the launcher-wheel e2e), the
  tray panel's own scroll code uses `self.scroll - dy * ROW_H` — if the
  tray ever scrolls on a real session, wheel-down would move the list UP.
- **Not tested**: a tray panel tall enough to scroll (a real session with
  many SNI items).
- **Next step**: when the tray panel next gets touched, align it with the
  Flickable convention (or port it onto the Flickable widget) and add a
  scroll e2e.

## KI-5: agenda calendar golden flaked once on a mid-reveal commit capture

- **Symptom**: `agenda_calendar_golden` failed once in the first full-suite
  run (31 tests) after passing its regen run and 3 isolated runs; the
  immediately following full-suite run was green.
- **Repro**: not reproduced since (1 in ~3 full-suite runs); the suspect is
  the `Capture::Commit(3)` frame landing at a slightly different reveal
  fraction under 31-way parallel e2e load (the reveal tween shifts the
  panel by up to 40 px and fades alpha — a frame of jitter can exceed the
  golden tolerance).
- **Ruled out**: the calendar math (unit-locked); the date line (stable
  within a day); leftover processes.
- **Fix applied**: the test now captures a TIMED frame (AtMs 2600 — after
  the ~460 ms reveal completes and after the tab click at 1600), which is
  deterministic; verified with 4 consecutive green runs. The other goldens
  keep Commit(3) (stable across many sessions — revisit per-test if any
  flakes again).
- **Not tested**: forcing the parallel-load jitter to reproduce the
  original failure (judged not worth the flakiness it would add).
