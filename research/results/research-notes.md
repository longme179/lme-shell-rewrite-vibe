# Spike research notes (Step 2.0) — citations

Checked 2026-10-11 via crates.io API (UA "lme-spike-research"), docs.rs
(`/crate/NAME/latest/`), GitHub/Codeberg repo metadata, and shallow clones of
release tags. Full research log in the session transcript; facts below are
the ones ADR 0001 relies on.

## Crate metadata (latest stable at check time)

| Crate | Version | Released | License | Repo last push | docs.rs | Downloads |
|---|---|---|---|---|---|---|
| smithay-client-toolkit | 0.21.1 | 2026-07-23 | MIT | 2026-09-04 | ok | ~65.4 M |
| wayland-client | 0.31.15 | 2026-07-22 | MIT | 2026-10-09 | ok | ~73.4 M |
| wayland-backend | 0.3.17 | 2026-08-14 | MIT | 2026-10-09 | ok | ~52.8 M |
| tiny-skia | 0.12.0 | 2026-02-02 | BSD-3-Clause | 2026-08-20 | ok | ~54.3 M |
| cosmic-text | 0.19.0 | 2026-04-22 | MIT OR Apache-2.0 | 2026-09-30 | ok | ~10.1 M |
| femtovg | 0.27.0 | 2026-08-31 | MIT OR Apache-2.0 | 2026-10-10 | ok | ~2.8 M |
| glutin | 0.32.3 | 2025-04-30 | Apache-2.0 | 2026-09-29 | ok | ~34.2 M |
| slint | 1.18.1 | 2026-09-21 | GPL-3.0-only OR Slint royalty-free/commercial | 2026-10-10 | ok | ~1.9 M |
| layer-shika | 0.3.1 | 2026-01-19 | AGPL-3.0-or-later | 2026-02-24 (Codeberg) | ok | ~2.3 k |
| iced_layershell | 0.19.1 | 2026-07-12 | MIT | 2026-10-04 | ok | ~79 k |
| softbuffer | 0.4.8 | 2025-12-13 | MIT OR Apache-2.0 | 2026-08-05 | ok | ~27.3 M |
| xkbcommon | 0.9.0 | 2025-08-09 | MIT | 2025-10-25 | ok | ~9.0 M |

## Key technical verifications (source-level)

- sctk 0.21.1 default features = pure-Rust wayland backend; `system` feature
  opts into libwayland (pkg-config). sctk's build.rs runs
  `pkg_config find("xkbcommon")` when its default `xkbcommon` feature is on.
- cosmic-text 0.19 defaults: harfrust 0.5 (pure-Rust HarfBuzz port) + swash
  0.2.6 + fontdb 0.23 whose "fontconfig" feature is pure-Rust
  (fontconfig-parser); no C library linkage.
- femtovg 0.27 text: rustybuzz shaping (`src/text/textlayout.rs`), glyphs as
  paths by default, optional swash image glyphs.
- glutin 0.32.3 EGL: libEGL located via libloading at runtime
  (`src/api/egl/mod.rs`: `["libEGL.so.1", "libEGL.so"]`); wayland-only
  feature set exists; wayland-egl also dlopen'ed.
- slint 1.18: `MinimalSoftwareWindow` + `SoftwareRenderer::render(buffer,
  pixel_stride)` public API; no layer-shell backend anywhere in the tree; no
  public font registration (fontique system db).
- niri v25.11 (niri-wm/niri, published 2025-11-30): layer-shell **v5**;
  on-demand surfaces focused on map (handlers/layer_shell.rs, "lxqt-runner"
  comment) and on pointer press (input/mod.rs → focus_layer_surface_if_on_demand);
  exclusive beats on-demand per layer; Bottom/Background exclusive only when
  no layout windows exist (src/niri.rs:1209–1213).
- wlr-layer-shell protocol: v5, canonical home wlroots/wlr-protocols
  (NOT wayland-protocols — verified in both repos); `on_demand` added v4
  (2021-01-06), `set_exclusive_edge` added v5 (2024-01-26).

## Sources

crates.io API (`/api/v1/crates/NAME`, `/api/v1/crates/NAME/reverse_dependencies`,
`/api/v1/crates/NAME/versions`), docs.rs build pages, GitHub API + raw files
(smithay/client-toolkit v0.21.1, slint-ui/slint v1.18.1, femtovg/femtovg
v0.27.0, glutin 0.32.3, niri-wm/niri v25.11 incl. smithay pin d743e1a3),
Codeberg API (waydeer/layer-shika), release tags cloned shallow and read,
gitlab.freedesktop.org wayland-protocols @ 682ae76 and wlr-protocols @
bf4fc79 (cloned; raw HTTP is Anubis-gated).
