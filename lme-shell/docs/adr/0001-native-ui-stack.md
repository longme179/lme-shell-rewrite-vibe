# ADR 0001: Native UI stack for the Phase 2 layer-shell clients

- Status: **Accepted** (owner decision 2026-10-11, PHASE2_AMENDMENT_1; see
  "Decision (owner)" below)
- Date: 2026-10-11 · Branch: `rewrite/phase-2`
- Decides: the rendering/input stack for `lme-powermenu` (Step 2.1) and
  `lme-launcher` (Step 2.2), both standalone Wayland layer-shell clients on
  niri 25.11 (Fedora 44, integrated Radeon, 1080p 120 Hz, no continuous blur
  shader, no discrete GPU).

## Context

Each QML panel costs memory inside the Quickshell process and Quickshell
creates per-output surfaces. Phase 2 replaces exactly two panels (power menu,
launcher) with small native clients that exist only while visible. The stack
choice is expensive to reverse, so three throwaway spikes were built and
measured against a purpose-built **mock Wayland compositor**
(`native/spike/mock-compositor`: wl_compositor/wl_shm/wl_output/wl_seat +
zwlr_layer_shell_v1 **v5**, keyboard with xkb keymap, 120 Hz frame-callback
pacing, buffer capture to PNG). All clients render the same 420×560
"power menu" panel: translucent rounded rectangle + Roboto title + 5 item
rows; Esc exits; `--animate` runs a continuous opacity pulse.

Cloud-sandbox limits (marked **needs real-machine validation** everywhere they
apply): no GPU/libEGL (spike B builds but cannot run), no real compositor
(frame pacing emulated), no real input hardware; container CPU/RSS numbers
are structure-comparable, not laptop-comparable.

## Verified ecosystem facts (checked 2026-10-11)

crates.io + docs.rs + repo activity checked per crate (versions, dates,
licenses, downloads, docs.rs build status). Research artifacts kept under
`native/spike/results/` and the session research log; key facts:

- **smithay-client-toolkit 0.21.1** (2026-07-23, MIT, ~65 M downloads) — pure-Rust wayland backend by default; layer-shell via `sctk::shell::wlr_layer`; `SlotPool` wl_shm buffers; keyboard via xkbcommon. Actively maintained (Smithay).
- **wayland-client 0.31.15 / wayland-backend 0.3.17** (2026-07/08, MIT) — the `client_system` feature exposes raw `wl_display*`/`wl_surface*` pointers needed for EGL interop.
- **tiny-skia 0.12.0** (2026-02-02, BSD-3-Clause) — CPU rasterizer, essentially dependency-free.
- **cosmic-text 0.19.0** (2026-04-22, MIT OR Apache-2.0) — defaults 100% pure Rust (harfrust shaping + swash rasterization + fontdb with pure-Rust fontconfig parsing). Vendored fonts via `fontdb::Source::Binary`.
- **femtovg 0.27.0** (2026-08-31, MIT OR Apache-2.0) — GLES renderer (glow), real shaping via rustybuzz; glyphs rendered as GPU paths by default.
- **glutin 0.32.3** (2025-04-30, Apache-2.0) — **dlopens libEGL at runtime** (build needs no EGL present); wayland-only feature set exists.
- **slint 1.18.1** (2026-09-21, GPL-3.0-only OR royalty-free OR commercial) — `renderer-software` + `MinimalSoftwareWindow` are stable public API for custom platforms; **no built-in layer-shell backend**; **no public font-registration API** in 1.18 (system fonts via fontique).
- **layer-shika 0.3.1** (2026-01-19, **AGPL-3.0-or-later**, ~2.3k downloads) — README: "early development … not yet ready for production"; repo idle since 2026-02. **Rejected as a dependency** (maturity + AGPL vs our GPL-3.0-or-later outbound).
- **iced_layershell 0.19.1** (MIT, healthy) — requires wayland-backend `client_system` (pkg-config libwayland at build); a whole-iced commitment; not spiked.

### niri v25.11 keyboard interactivity (verified from v25.11 source)

niri advertises **zwlr_layer_shell_v1 version 5** and recomputes focus in
`update_keyboard_focus()` (src/niri.rs) with priority: lock/exit-confirm UI →
layer-shell popup grabs → **exclusive** surfaces on Overlay then Top →
**on-demand** focus (Top, then Bottom/Background) → regular windows.

- `Exclusive` (Overlay/Top): always in the focus chain while mapped; first
  mapped exclusive wins if several exist; on close, focus falls through the
  chain automatically. **Use for the power menu** (modal grab).
- `OnDemand`: niri focuses the surface **automatically when it is first
  mapped** (code comment: "Some launchers like lxqt-runner rely on this",
  src/handlers/layer_shell.rs) **and on click**; the recorded focus is
  re-validated every refresh. **Use for the launcher** (modal-but-polite).
- `None`: never receives keyboard focus.
- v25.11 release notes confirm layer-shell v5 (`set_exclusive_edge`) and
  fixed Qt layer-shell popup grabs. *(Focus "feel" with real typing remains
  needs-real-machine validation.)*

## Candidates and measurements

All numbers: release builds, mock compositor at 120 Hz frame-callback pacing,
container CPU; RSS via /proc sampling (n=37 over ~9.3 s); CPU% measured after
the first frame; "anim" = continuous full-panel redraw (A/B: every frame
callback ≈120 Hz; C: its own 16 ms Timer ≈62 Hz). Raw table:
`native/spike/results/measurements.md`; rendered evidence:
`results/capture-spike-cpu.png`, `results/capture-spike-slint.png`.

| | **A. sctk + tiny-skia + cosmic-text** (CPU/wl_shm) | **B. sctk + glutin + femtovg** (EGL/GLES) | **C. sctk + Slint software renderer** |
|---|---|---|---|
| clean build | 106 s | 53 s | 149 s |
| binary raw / stripped | 5.9 MB / 4.6 MB | 3.7 MB / 2.8 MB | 10.8 MB / 9.0 MB |
| RSS idle (shown, no anim) | 7.8 avg / 8.4 peak MB | *cloud: n/a* | 7.4 avg / 7.6 peak MB |
| RSS while animating | 8.7 MB | *cloud: n/a* | 8.6 MB |
| CPU% idle | **0.0%** | *cloud: n/a* | 1.4% (periodic internal activity) |
| CPU% animating | 9.3% @120 Hz | *cloud: n/a* | 5.8% @62 Hz (≈0.09%/frame vs A's ≈0.08%/frame) |
| cold start (bind→first frame) | 7–9 ms | *cloud: n/a* | 2–3 ms |
| Rust LOC (panel+text+keys) | 586 | 466 | 453 + 64 lines .slint markup |
| Roboto text | ✓ vendored VF via cosmic-text (harfrust+swash AA; pixel-verified) | ✓ `add_font_mem` (runtime untested in cloud) | ✗ fell back to a system font — **no public font-registration API in 1.18** |
| Extra runtime deps | none (wl_shm only) | libEGL + GLES (dlopen), wayland `system` backend | none (wl_shm only) |
| Licensing | MIT/BSD/Apache — fits GPL-3.0-or-later | fits | **GPL-3.0-only election** required for the combined work |

## Findings per candidate

**A (tiny-skia + cosmic-text).** Everything is measurable in the cloud; zero
CPU while idle; the only stack whose text output (real Roboto VF, AA, proper
shaping) is pixel-verified here. Cost: hand-written compositing (~150 LOC:
rounded-rect path, glyph blit, premultiplied blend) and, later, damage
tracking; longest clean build of the three (harfrust/swash/skrifa tree is
heavy, but one-time per machine).

**B (femtovg + EGL).** Smallest binary, least layout code, GPU-accelerated
paths, the natural fit for future effects. But it is **unvalidatable in this
sandbox** (no libEGL): choosing B means committing before any runtime
evidence, and it adds the EGL/GLES runtime requirements the README explicitly
avoids needing. Retained in-tree as a complete, compiling reference
(`native/spike/spike-gpu/`) and as the escalation path if CPU rendering of
the launcher list proves too slow on the real machine.

**C (Slint).** Best ergonomics (declarative markup, built-in layout/focus).
Three real integration costs surfaced by the spike:
1. `blocking_dispatch` is incompatible with Slint timers — a custom **pump
   loop** (poll + `update_timers_and_animations` +
   `duration_until_next_timer_update`) is required (~35 LOC; implemented).
2. After timer-driven imperative property changes, `MinimalSoftwareWindow`'s
   `needs_redraw` was not set automatically in this setup — the spike calls
   `window().request_redraw()` explicitly from the callback.
3. No public API to register a bundled font in 1.18 (fontique system fonts
   only) — a visual-parity blocker for the Material/glass look — and adopting
   Slint requires electing GPL-3.0-only for the combined work.

**Non-Rust candidate (C++ as Noctalia v5).** Considered, not measured: the
~6× per-monitor memory factor comes from dropping the Qt runtime, not the
language (Phase 0 finding); this sandbox cannot fairly build/measure a C++
Wayland+Skia stack (no dev headers, no sudo); the Rust sctk ecosystem is
mature and license-clean here. A C++ spike can be added on request but needs
a different environment.

## Decision

**Recommendation: A — sctk 0.21 + tiny-skia + cosmic-text, wl_shm CPU
rendering**, with B's EGL code kept in-tree as the documented escalation
path (the surface/seat scaffolding is shared, so the renderer stays
swappable behind `lme-common` once Step 2.2 extracts it).

Rationale: the only candidate fully validated in the cloud (text-quality
evidence attached), 0% idle CPU, no GPU/runtime dependencies beyond wl_shm,
license-clean dependencies, and its downsides (hand compositing, no GPU
effects) are acceptable for two small, transient panels with no continuous
effects. Per-frame CPU is on par with Slint; the 120 Hz full-panel redraw
figure (9.3% of one container core) is an upper bound — real clients will
damage-track, and the panels are short-lived.

Preliminary answer to the 2.1 spawn-vs-daemon question (finalized with
real-machine numbers in 2.1): cold start is 2–9 ms bind→map in the
container — **spawn-on-demand** is clearly viable and costs no resident
memory.

## Consequences

- `lme-powermenu` (2.1) and `lme-launcher` (2.2) build on A; shared
  surface/theming/config code is extracted into `lme-common` only at 2.2
  (per plan: abstracting from one example guesses wrong).
- Crate one-liners (also in each Cargo.toml): sctk (layer-shell+seat+shm
  plumbing), wayland-client (connection/event queues; `system` feature only
  on the B-path), tiny-skia (raster), cosmic-text (fonts+shaping), rustix
  (poll/flock without libc boilerplate). B-path adds glutin (EGL context) +
  femtovg (GLES vector renderer). C-path (slint) rejected.
- Choosing C instead requires the owner to accept: GPL-3.0-only election,
  system-font-only text (or waiting for a public font API), and the pump +
  manual-redraw pattern documented above.

## Open questions for the owner

1. Stack: A (recommended), B (validate on real machine first), or C (accept
   the three integration costs + licensing)?
2. If A: damage-track from the start (more code) or full-redraw per frame
   while shown (simpler; panels are transient)?

## Decision (owner, 2026-10-11 — PHASE2_AMENDMENT_1)

**Accepted: Option A — smithay-client-toolkit 0.21.1 + tiny-skia 0.12.0 +
cosmic-text 0.19.0 (the exact spike versions; pin them in every Phase 2
client), wl_shm CPU rendering. Clients are spawn-on-demand** (cold start
7–9 ms bind→map makes on-demand spawning free). Options B and C are not
chosen; B's EGL code stays in-tree as the documented escalation path only.

Reasons recorded by the owner: full cloud validation of A (pixel-verified
Roboto text, 0% idle CPU, no GPU/runtime dependencies), license-clean
dependencies, and transient small panels that do not need GPU effects.

Riding along with the acceptance:

- **Rendering: full redraw of the panel every frame while visible; no
  damage tracking.** Revisit only if profiling shows a problem; record the
  numbers if so. (Spike reference: 9.3% of one container core at a constant
  120 Hz full-panel redraw — an upper bound.)
- **Keyboard interactivity (from niri v25.11 source): power menu =
  `Overlay` + `Exclusive`; launcher = `Overlay` + `OnDemand`.** The focus
  *feel* with real typing remains **needs real-machine validation**.
