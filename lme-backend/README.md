# lme-backend

Native Rust replacement for `scripts/desktop_backend.py` (and
`scripts/clipboard.py`), speaking the same JSON-lines protocol on stdio so
the QML shell can switch backends with `LME_SHELL_BACKEND=rust` while the
Python implementation remains the default fallback.

One binary, selected by flags:

| Invocation | Role |
|---|---|
| `lme-backend [--demo] [--settings] [--interval S]` | state daemon (JSON lines on stdio) |
| `lme-backend --once …` | one read-only snapshot, then exit |
| `lme-backend --restore-wallpaper FILE` | apply wallpaper via awww, exit |
| `lme-backend --clipboard [--directory D]` | clipboard history daemon |
| `lme-backend --capture text\|image [--directory D]` | one-shot clipboard capture (stdin payload) |

## Crate policy (one line each)

- `serde` / `serde_json` (`preserve_order`) — the wire protocol *is* JSON;
  insertion-ordered maps reproduce Python dict key order in `settings.json`.
- `rustix` — `flock`/`fsync`/file-mode syscalls without unsafe libc blocks.
- `ureq` (rustls, no default features) — smallest adequate blocking HTTPS
  client for the bounded weather requests; no async runtime needed.
- `rusqlite` (bundled SQLite) — byte-compatible `history.sqlite3` clipboard
  storage shared with the Python service; bundling avoids a system sqlite
  devel dependency at build time.
- `sha2` — clipboard entry ids `sha256(mime ++ "\0" ++ data)`, same digest
  as the Python service.
- Deliberately **not** used in Phase 1:
  - `niri-ipc` — the protocol passes niri's JSON through untouched and sends
    exactly one hand-shaped action; pinning date-versioned upstream types
    would risk version skew with the compositor, so the socket speaks
    opaque `serde_json::Value` like the Python code does.
  - `zbus` — Phase 1 keeps the same CLI subprocesses (`busctl`, `nmcli`,
    `wpctl`, `brightnessctl`, `powerprofilesctl`) for exact behavioural
    parity; migrating hot probes to D-Bus is a measured Phase 2 step.
  - `tokio`/async — the daemon is two bounded worker threads + a poll loop,
    exactly like the Python design.

## Build

```bash
cargo build --release          # binary: target/release/lme-backend
cargo fmt && cargo clippy -D warnings && cargo test
```

`scripts/build-backend.sh` wraps this for the shell integration; the
parity suite (`tests/parity.rs`) runs the Python backend in an identical
fixture environment when `python3` is available.

## Selecting the Rust backend (Phase 1 switch)

The QML shell picks the backend with the `LME_SHELL_BACKEND` environment
variable (read in `qml/services/ShellState.qml` and
`qml/services/ClipboardState.qml`):

```bash
scripts/build-backend.sh                 # release build into target/release/
LME_SHELL_BACKEND=rust qs -c lme-shell   # run the shell on the Rust daemon
```

Any other value (or unset) keeps the Python backend, which remains the
default until one real-machine session with the Rust backend is clean.
`scripts/check.sh --rust` runs the Python suite plus the full Rust gate
(fmt + clippy -D warnings + test) in one command.

## Known deliberate deviations from the Python wire behavior

Both are unobservable for the QML client (it only sends well-formed UTF-8
JSON) and are recorded here per the contract rules:

1. The clipboard daemon answers malformed JSON with the fixed message
   `Invalid JSON input`; Python surfaces its `json.JSONDecodeError` text
   (e.g. `Expecting value: line 1 column 1 (char 0)`). The desktop-state
   daemon uses the same fixed message in both implementations.
2. Invalid UTF-8 on the clipboard daemon's stdin is replaced lossily;
   Python rejects it with a `UnicodeDecodeError` message. Either way the
   line fails to parse and produces an error `lastResult`.

## Real-machine validation backlog

Event-stream behavior under the real compositor, wl-paste watchers, and a
full session with `LME_SHELL_BACKEND=rust` — see
`docs/REWRITE_PLAN.md` (Phase 1 exit criteria).
