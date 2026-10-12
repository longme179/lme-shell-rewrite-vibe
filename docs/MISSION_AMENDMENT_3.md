# Amendment 3: sandbox survival + Step 0 interaction work (2026-10-12)

> [RECONSTRUCTED 2026-10-13 from the session context of the chat that carried
> it — the verbatim text was not retained across context loss. Substance is
> faithful; wording is not guaranteed. Read together with
> docs/MISSION_CONTINUATION.md and docs/MISSION_AMENDMENT_2.md; on conflict,
> later amendments win.]

1. First action, regardless of tree state (even mid-edit, even non-compiling):
   run `scripts/gh-sync.sh` so the current work is snapshotted to the backup
   repo's `wip/` ref. WIP snapshots do not need to compile.
2. Keep running `scripts/gh-sync.sh` on a ~10-minute cadence for the rest of
   the session.
3. Record the sandbox lessons in STATE.md: run cargo in the foreground, keep
   individual commands under ~5 minutes (the sandbox kills long foreground
   jobs), and run `cargo fetch` before building so the network phase is
   separated from the compile phase.
4. Work items a–e (Step 0a — pointer/interaction semantics against the mock):
   a. bar module hover + click buttons (audio/network/battery pills with
      hover states and click actions)
   b. launcher interaction (hover, click-to-launch, typing)
   c. CenteredPanel coordinate fix (launcher positioned in compositor
      coordinates, not output-local)
   d. scroll dual-path: wheel (value120 notches) vs touchpad (pixel deltas,
      stop events)
   e. workspace scroll (focus up/down) + dry-run volume OSD
5. Step 0b: vsync pacing measurement against the mock compositor
   (frame-callback grid), results into docs/PERF.md.
