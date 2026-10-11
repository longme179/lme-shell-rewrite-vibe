#!/usr/bin/env bash
# scripts/bench-native.sh — measure the native daemon against the mock
# compositor: idle CPU/wakeups, RSS, cold start, animation fps, p99 render.
# Results are appended to docs/PERF.md. Sandbox CPU (cloud) — label as such.
set -u
cd "$(dirname "$0")/.."
. scripts/env.sh

T=/tmp/lme-bench
rm -rf "$T" && mkdir -p "$T/runtime" "$T/config/lme-shell"
echo '{"animations": false}' > "$T/config/lme-shell/settings.json"
printf '{"available": true, "workspaces": [{"id": 1, "idx": 1, "name": "", "output": "bench", "is_focused": true}], "windows": [{"id": 10, "title": "bench window", "app_id": "kitty", "is_focused": true, "workspace_id": 1}]}' > "$T/fake.json"
echo '{}' > "$T/fakesys.json"

run_daemon () { # $1 = socket, $2 = duration, extra args after
  local sock="$1"; shift
  local dur="$1"; shift
  XDG_RUNTIME_DIR="$T/runtime" WAYLAND_DISPLAY="$sock" \
    "$HOME/target/release/mock-compositor" --mode idle --duration "$dur" \
    --output 1280x800 --socket "$sock" "$@" > "$T/mock.log" 2>&1 &
  MOCK=$!
  sleep 0.4
  XDG_RUNTIME_DIR="$T/runtime" XDG_CONFIG_HOME="$T/config" HOME="$T" \
    WAYLAND_DISPLAY="$sock" LME_SHELL_FAKE_STATE="$T/fake.json" \
    LME_SHELL_FAKE_STATE_SYSTEM="$T/fakesys.json" \
    "${LME_SHELL_BIN:-$HOME/target/release/lme-shell}" daemon --dry-run "$@" > "$T/shell.log" 2>&1 &
  DAEMON=$!
  # wait for the daemon socket
  for _ in $(seq 1 100); do
    [ -S "$T/runtime/lme-shell.sock" ] && break
    sleep 0.1
  done
}

stop_all () {
  kill $DAEMON $MOCK 2>/dev/null
  wait $DAEMON $MOCK 2>/dev/null
}

proc_stat_fields () { # pid -> utime stime (clock ticks)
  awk '{print $14, $15}' "/proc/$1/stat" 2>/dev/null || echo "0 0"
}
voluntary_ctxt () {
  awk '/^voluntary_ctxt_switches:/ {print $2}' "/proc/$1/status" 2>/dev/null || echo 0
}
nonvoluntary_ctxt () {
  awk '/^nonvoluntary_ctxt_switches:/ {print $2}' "/proc/$1/status" 2>/dev/null || echo 0
}
rss_kb () {
  awk '/^VmRSS:/ {print $2}' "/proc/$1/status" 2>/dev/null || echo 0
}

echo "== lme-shell native bench (sandbox cloud CPU) =="

# ---- 1. Idle: 60 s with the bar only (animations off, no battery module
#         data: fakesys empty) ------------------------------------------
run_daemon bench-idle 70 --capture-commit 2 --capture-out "$T/bar.png"
sleep 2   # let startup settle
IDLE_PID=$(pgrep -f "lme-shell daemon" | head -1)
read -r U1 S1 <<< "$(proc_stat_fields $IDLE_PID)"
V1=$(voluntary_ctxt $IDLE_PID); N1=$(nonvoluntary_ctxt $IDLE_PID)
sleep 60
read -r U2 S2 <<< "$(proc_stat_fields $IDLE_PID)"
V2=$(voluntary_ctxt $IDLE_PID); N2=$(nonvoluntary_ctxt $IDLE_PID)
RSS=$(rss_kb $IDLE_PID)
HZ=$(getconf CLK_TCK)
CPU_TICKS=$(( (U2 - U1) + (S2 - S1) ))
WAKEUPS=$(( (V2 - V1) + (N2 - N1) ))
echo "idle_60s_cpu_ticks=$CPU_TICKS (HZ=$HZ)"
echo "idle_60s_wakeups=$WAKEUPS (avg $((WAKEUPS / 60))/s: voluntary=$((V2-V1)) nonvoluntary=$((N2-N1)))"
echo "idle_rss_kb=$RSS"
stop_all

# ---- 2. Cold start (bind->map from the compositor MAPPED line) ---------
run_daemon bench-cold 8
sleep 3
stop_all
COLD=$(grep -o 'bind->map [0-9]*ms' "$T/mock.log" | head -1 | grep -o '[0-9]*')
echo "cold_start_bind_to_map_ms=$COLD"

# ---- 3. Animation: reveal at 120 Hz, fps from SUMMARY + p99 render ------
echo '{"animations": true}' > "$T/config/lme-shell/settings.json"
LME_SHELL_PERF=1 run_daemon bench-anim 8
sleep 1.5
XDG_RUNTIME_DIR="$T/runtime" "${LME_SHELL_BIN:-$HOME/target/release/lme-shell}" msg toggle power > /dev/null
sleep 5
stop_all
# p99 render time from the daemon perf log (LME_SHELL_PERF set via run_daemon "$@")
P99=$(grep -o 'ms=[0-9.]*' "$T/shell.log" | grep -o '[0-9.]*' | sort -n | awk '{a[NR]=$1} END {if (NR>0) print a[int(NR*0.99)+((NR*0.99<1)?0:0)]; if (NR==0) print ""}')
P99=$(grep -o 'ms=[0-9.]*' "$T/shell.log" | grep -o '[0-9.]*' | sort -n | awk '{a[NR]=$1} END {print (NR>0) ? a[int((NR*99+99)/100)] : ""}')
echo "anim_render_count=$(grep -c RENDER "$T/shell.log") p99_render_ms=$P99"
# With --mode idle frame callbacks fire immediately, so the animation runs
# unthrottled; the fps here measures the render loop's ceiling, not vsync.
FRAMES=$(grep -o '"frames_fired":[0-9]*' "$T/mock.log" | head -1 | cut -d: -f2)
DUR_MS=$(grep -o '"duration_ms":[0-9]*' "$T/mock.log" | head -1 | cut -d: -f2)
if [ -n "$FRAMES" ] && [ -n "$DUR_MS" ] && [ "$DUR_MS" -gt 0 ]; then
  echo "anim_frames_fired=$FRAMES over ${DUR_MS}ms"
fi

echo "== done (append these numbers to docs/PERF.md) =="
