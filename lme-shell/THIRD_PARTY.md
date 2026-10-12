# Attribution and licenses

LME Shell is an original Fedora and niri implementation inspired by the desktop
interaction patterns of [end-4/dots-hyprland](https://github.com/end-4/dots-hyprland).
The upstream reference inspected during development was commit
`c4f0582cbf4ad29607dbf3c20337815de8690b34`. Its root license is GNU GPL version 3.
No end-4 source code, wallpapers, bundled fonts, icons, or installation scripts
have been copied into this project. Original project source is distributed
under GPL-3.0-or-later; the motion adaptation below retains GPL-3.0-only.
The full GNU GPL version 3 license is in [LICENSE](LICENSE).

Quickshell, Qt, niri, and the desktop utilities are external runtime dependencies
with their own licenses. Installing this project does not redistribute them.

The color system follows Dracula Pro's dark, purple-led visual direction. It does
not include purchased Dracula Pro files, wallpapers, fonts, or branding assets.
Palette values are editable locally; users who own a preferred Pro variant can
enter their own color values. The project is unaffiliated with Dracula Pro.

The included wallpaper and UI shapes are original vector artwork. Application
icons are resolved from the user's installed icon theme, and are not bundled.

The modular bar, searchable settings registry and Material design direction also
reference [snowarch/iNiR](https://github.com/snowarch/iNiR), inspected at commit
`48cdcecb0f6089cfbb7019054602eab28e154b3b` (root license: GNU GPL version 3).
These interaction patterns have been implemented independently for LME Shell,
with the compatible motion adaptations described below. No iNiR shaders, bundled
fonts, wallpapers or installation scripts are redistributed.
See [docs/INIR_PORT.md](docs/INIR_PORT.md) for the settings/bar mapping.

Roboto is installed through Fedora's `google-roboto-fonts` package. The project
references it by family name and does not distribute the font files. Small shell
icons are original Canvas drawings; Material Symbols font assets are not bundled.


Wallpaper rendering uses the external [awww](https://codeberg.org/LGFae/awww.git)
client and daemon. No upstream binary or source is bundled. The CLI integration
was checked against release 0.12.1 at
`f66e12a76dbc4c669b2f1375f78bce49f5b19d66` (GNU GPL version 3).

Weather and city search are provided by [Open-Meteo](https://open-meteo.com/),
whose weather data requires attribution under CC BY 4.0. The widget displays
that attribution. Approximate automatic location uses [IPwho.is](https://ipwho.is/).
These are external network services; no provider dataset or assets are bundled.
The implementation uses the providers' public endpoints without an API key.

Clipboard history and the expandable Activity island also reference
[AvengeMedia/DankMaterialShell](https://github.com/AvengeMedia/DankMaterialShell),
inspected at `f372777829212f0db41dc623e7e495e63c8e4341` (MIT), and iNiR at the
revision above. All delivered history, storage, Activity and protocol-test code
is original. Those feature implementations do not redistribute upstream source,
assets or binaries. The later shared motion adaptations are described below.
`wl-clipboard` is an external GPL-3.0-or-later runtime dependency.

The desktop upgrade workflows were reviewed against iNiR notification grouping
and persistence, and DankMaterialShell launcher, network and niri display APIs.
Their implementation source was not vendored. The bundled emoji/symbol names
are generated with Python standard-library `unicodedata`; search aliases are
written for this project. The repository also includes the owner-supplied MIT
Ponytail skill, with its original license declaration preserved.

Shared motion now adapts these upstream implementations:

- [Caelestia StateLayer.qml](https://github.com/caelestia-dots/shell/blob/21a75375bb33123e481e796a74965038847c503d/components/StateLayer.qml),
  plus `components/Anim.qml` and `components/controls/ButtonBase.qml`, at
  `21a75375bb33123e481e796a74965038847c503d` (GPL-3.0). Press/hold/release
  sequencing is adapted in `qml/components/LiquidFeedback.qml` under
  GPL-3.0-only. The implementation uses a bounded flat pill ripple and LME's
  controls; it does not require Caelestia's plugin, shapes or assets.
- [DankMaterialShell Anims.qml](https://github.com/AvengeMedia/DankMaterialShell/blob/a1e6f6d6acae5e0688a4f04bd0e584b201ff6f68/quickshell/Common/Anims.qml)
  at `a1e6f6d6acae5e0688a4f04bd0e584b201ff6f68` (MIT). Expressive fast spatial,
  effects and exit Bézier curves are ported into `qml/theme/Theme.qml`; spatial
  overshoot is scaled by LME's existing bounce preference. Copyright (c) 2025
  Avenge Media LLC. The complete permission/copyright notice is preserved in
  [third_party/DankMaterialShell-LICENSE](third_party/DankMaterialShell-LICENSE).
- iNiR `modules/common/widgets/RippleButton.qml` at the revision above (GPL-3.0)
  informs the hue-preserving transparent hover state and shared feedback.

The ports reuse LME's preferences, controls and panel loader. No upstream binary,
plugin, font or wallpaper is bundled. Native blur remains implemented through
Quickshell/niri's existing compositor protocol.

Night Light also references iNiR's niri/wlsunset workflow. `wlsunset` and
`tesseract` are external runtime tools, not bundled binaries. Sunrise/sunset
calculation uses the approximate algorithm from *Almanac for Computers* (1990),
described in Ed Williams' [Sunrise/Sunset Algorithm](https://edwilliams.org/sunrise_sunset_algorithm.htm).
The Python implementation is original; it is not an astronomical ephemeris or
display calibration system.

## Phase 2 native-client attribution

Project license: GPL-3.0-or-later (see README). This section records every
third-party item adapted, vendored or consulted for the native Phase 2 clients,
per the Phase 2 rules ("check the license of any third-party code you adapt and log it").

### Adapted code

| What | License | Source | Use |
|---|---|---|---|
| Structure of `examples/simple_layer.rs` (layer-shell client skeleton: registry/seat/output states, `KeyboardHandler`, frame-callback flow, `SlotPool` buffer cycle) | MIT | smithay-client-toolkit v0.21.1 (github.com/Smithay/client-toolkit, tag v0.21.1) | Adapted in `native/spike/spike-cpu`, `spike-gpu`, `spike-slint/src/main.rs` (throwaway Phase 2 spikes; the same skeleton will seed `lme-powermenu`/`lme-launcher`) |
| EGL-on-layer-shell creation pattern (`Display::new` over `RawDisplayHandle::Wayland`, config template, `create_window_surface`, `OpenGl::new_from_function_cstr`) | MIT OR Apache-2.0 (glutin 0.32.3); study of layer-shika (AGPL-3.0-or-later, **read-only, no code copied**) | glutin examples + femtovg `examples/helpers/opengl.rs` (MIT OR Apache-2.0) | `native/spike/spike-gpu/src/main.rs` |

### Vendored assets

| What | License | Source | Use |
|---|---|---|---|
| `native/spike/assets/fonts/Roboto-VF.ttf` (variable font, 478 KB) | SIL Open Font License 1.1 (Google Fonts repo `ofl/roboto`) | raw.githubusercontent.com/google/fonts (Apache-2.0-licensed repo; font itself OFL-1.1, permits bundling) | Deterministic Roboto rendering in spike A (cosmic-text) and spike B (femtovg) |
| `native/protocols/wlr-layer-shell-unstable-v1.xml` (v5, reference copy) | MIT (wlroots/wlr-protocols repo) | gitlab.freedesktop.org/wlroots/wlr-protocols | Documentation/reference only; builds use the copy embedded in the `wayland-protocols-wlr` crate |

### Consulted (read-only, no code reused)

| What | License | Why consulted |
|---|---|---|
| niri v25.11 source (niri-wm/niri, tag v25.11) | GPL-3.0-only | Verified layer-shell keyboard-interactivity behavior (exclusive/on-demand focus rules) for docs/adr/0001; no code copied |
| layer-shika 0.3.1 (Codeberg waydeer/layer-shika) | AGPL-3.0-or-later | Evaluated as Slint+layer-shell integration; rejected (early development, AGPL, near-zero adoption); EGL pattern studied only |
| cosmic-text 0.19.0, tiny-skia 0.12.0, femtovg 0.27.0, slint 1.18.1, sctk 0.21.1 crate sources | MIT / MIT OR Apache-2.0 / GPL-3.0-only OR Slint licenses (slint) | API verification for the spikes (documented in docs/adr/0001) |

### License decision needed (owner)

Adopting **slint** would require electing its GPL-3.0-only option for the
combined work (the project is currently GPL-3.0-or-later; electing GPL-3.0
makes the combined distribution effectively GPL-3.0-only). This is an owner
decision — see docs/adr/0001 §Licensing.
