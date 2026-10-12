# Phase 2 stack spikes (Step 2.0) — throwaway measurement harness

These four crates exist to choose the native UI stack (see
`docs/adr/0001-native-ui-stack.md`). They are **not** shipped; the real
clients (`lme-powermenu`, `lme-launcher`) are built in Steps 2.1/2.2 from the
chosen stack. Keep them in-tree until Phase 2 is accepted, then decide
whether to prune.

| Crate | What |
|---|---|
| `mock-compositor` | Headless Wayland compositor: wl_compositor/wl_shm/wl_output/wl_seat(keyboard + keymap), zwlr_layer_shell_v1 v5, frame-callback pacing (idle/animate/keys modes), Nth-commit PNG capture |
| `spike-cpu` | Candidate A: sctk + tiny-skia + cosmic-text over wl_shm |
| `spike-gpu` | Candidate B: sctk + glutin(EGL, dlopen) + femtovg — builds in the cloud, runtime needs libEGL |
| `spike-slint` | Candidate C: sctk + Slint software renderer + custom pump event loop |

## Build (in this sandbox)

```bash
export PATH="$HOME/.cargo/bin:$PATH"
export PKG_CONFIG_PATH=/home/z/work/pkgconfig-fake/lib/pkgconfig
export RUSTFLAGS="-L /home/z/work/pkgconfig-fake/lib"
cargo build --release    # in each crate dir
```

## Run a scenario

```bash
export XDG_RUNTIME_DIR=/tmp/lme-mock-run; mkdir -p $XDG_RUNTIME_DIR
./mock-compositor/target/release/mock-compositor \
  --mode keys --duration 30 --socket t1 \
  --capture-commit 2 --capture-out results/capture.png > results/mock.log 2>&1 &
WAYLAND_DISPLAY=t1 ./spike-cpu/target/release/spike-cpu   # add --animate for the anim scenario
```

Modes: `idle` (frame callbacks immediate; client sleeps after first frame),
`animate` (callbacks paced at `--fps`), `keys` (scripted p/o/w/e/r/
BackSpace/Down/Up/Escape after map, then ends ~3 s later).

## Measurements

`./run-bench.sh` (add `MEASURE_BUILDS=1` to also time clean builds) writes
`results/measurements.md`. Numbers and PNG evidence are cited by ADR 0001.

## Known limits

- Everything here runs against the mock compositor, not niri — all runtime
  numbers are "needs real-machine validation".
- `spike-gpu` cannot create an EGL context in this sandbox (no libEGL); it
  exits cleanly with a message instead.
- The mock's keyboard sends a fixed key script; pointer is not implemented
  (layer-shell keyboard interactivity is what Phase 2 needs).
