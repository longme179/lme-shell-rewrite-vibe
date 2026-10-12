# Amendment 5: unstick and refocus (2026-10-13)

Read together with the continuation prompt and Amendments 2–4. Where they
conflict, this one wins.

The previous log ended inside fractional-scale debugging, about 40 tool calls
into a task time-boxed to one work unit. The MPRIS flake took about 100 calls.
Tier 2 had still not started at that point. New rules:

# Debugging cap (applies to every bug from now on)

- When one bug has used about 20 tool calls without an established root cause,
  STOP. Apply the smallest safe workaround. Record it in docs/KNOWN_ISSUES.md:
  symptom, repro command, what you ruled out, what you did not test, the
  workaround. Then move on. Write "root cause" or "upstream bug" in docs only
  when a minimal repro demonstrates it; otherwise write "hypothesis".
- Before each experiment, state in one line which result would disprove your
  current theory. Remove the code of theories that failed.

# Fractional scale: one last attempt, then freeze

Hard cap: 20 more tool calls. Client finding: the client's buffer is correct
but the mock reads other bytes. Mock-side hypotheses, in this order:

1. `wl_shm_pool.resize`: scaled buffers are bigger, so sctk grows the pool.
   The mock must re-mmap on resize and read from the current mapping.
2. Per-buffer parameters: the mock must use offset, stride, width and height
   from that wl_buffer's own `create_buffer` call, not from an earlier buffer
   or the surface size.
3. Which buffer: capture must read the buffer attached in the captured commit.
   sctk reuses released slots, so a later frame may already have overwritten
   it. Log buffer identity and offset at commit time and at capture time.

Safety design, required however this ends: at scale exactly 1.0 (no
fractional-scale event, or preferred scale 1.0) use the validated TIER-1 path
unchanged, byte for byte. Engage the viewport path only when the compositor
reports another scale. If it still fails after the cap, keep the code behind a
cargo feature `fractional-scale` (off by default), list it in KNOWN_ISSUES.md
as "unvalidated, needs a real niri test at 1.25", and say so in the README.
Never enable an unvalidated rendering path by default.

# Hygiene fixes (before Tier 2)

1. Remove code from failed theories (`keep_executor_alive`, the custom
   executor tick loop, the no-cache proxy helper) unless a test shows it is
   needed. Keep only what the final MPRIS fix needs, each with a one-line
   comment saying why. Mark D25 and ZBUS_NOTES as "hypothesis, not reproduced
   in a minimal probe" where that is true.
2. Test count fell from 137 to 134. Explain which tests were removed and why
   in STATE.md, or restore them.
3. Fix leaked test processes: start the daemon, mock and fakes in their own
   process group, kill and wait for them in Drop and on panic, and make each
   e2e test fail at start with a clear message if leftover lme-shell, mock or
   fake processes exist.
4. The daemon died at startup when `inotify_init` failed (EMFILE). It must
   survive: log once, disable hot reload, and check settings and palette
   mtimes every 5 s while the shell runs. Add a test that triggers the failure
   through a trait or an env switch, and list the 5 s poll in docs/PERF.md.
5. At each tier end, run every e2e test 10 times. A flake becomes a
   KNOWN_ISSUES entry or a fix, never a retry-until-green.

# Then

Run the gates, commit, run gh-sync, and continue with Tier 2 in the order from
Amendment 4, starting with TIER-2A. An image-viewing skill was used earlier;
inspect the golden PNGs for visual defects before each delivery.
