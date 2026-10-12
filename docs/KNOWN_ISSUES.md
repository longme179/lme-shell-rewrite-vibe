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
