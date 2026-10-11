#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Throwaway Phase 2 stack-spike measurement harness (step 2.0).
# Measures, for each runnable spike (cpu, slint; gpu is build-only in this sandbox):
#   - startup latency (bind -> first frame commit, from the mock compositor log)
#   - steady-state RSS while idle (no redraws after the first frame)
#   - peak RSS + CPU% while animating at 120 fps frame callbacks
#   - binary size (raw and stripped)
# With MEASURE_BUILDS=1 it also times clean release builds (destructive: cargo clean).

set -u
SPIKE_DIR="$(cd "$(dirname "$0")" && pwd)"
RESULTS="$SPIKE_DIR/results"
RUNTIME_DIR="${XDG_RUNTIME_DIR:-/tmp/lme-mock-run}"
mkdir -p "$RESULTS" "$RUNTIME_DIR"
export XDG_RUNTIME_DIR="$RUNTIME_DIR"

CLK_TCK=$(getconf CLK_TCK 2>/dev/null || echo 100)

read_stat_field() { awk -v f="$1" '{print $f}' "/proc/$2/stat" 2>/dev/null; }

rss_kb() { awk '/VmRSS/ {print $2}' "/proc/$1/status" 2>/dev/null; }
hwm_kb() { awk '/VmHWM/ {print $2}' "/proc/$1/status" 2>/dev/null; }

cpu_ticks() {
    local u t
    u=$(read_stat_field 14 "$1")
    t=$(read_stat_field 15 "$1")
    echo $((u + t))
}

run_scenario() { # name spike_bin mode duration extra_client_args...
    local name="$1" bin="$2" mode="$3" duration="$4"; shift 4
    local sock="lme-bench-$name-$$"
    rm -f "$RUNTIME_DIR/$sock"
    local mock_log="$RESULTS/mock-$name.log"

    "$SPIKE_DIR/mock-compositor/target/release/mock-compositor" \
        --mode "$mode" --duration "$duration" --fps 120 --socket "$sock" \
        >"$mock_log" 2>&1 &
    local mock_pid=$!
    sleep 0.4

    WAYLAND_DISPLAY="$sock" "$bin" "$@" >"$RESULTS/client-$name.log" 2>&1 &
    local pid=$!

    # settle: wait until the mock saw the first frame commit
    for _ in $(seq 1 100); do
        grep -q MAPPED "$mock_log" 2>/dev/null && break
        sleep 0.05
    done
    local settle_ticks settle_wall
    settle_ticks=$(cpu_ticks "$pid")
    settle_wall=$(date +%s%N)
    local n=0 sum=0 peak=0 last_ticks=$settle_ticks last_wall=$settle_wall
    while kill -0 "$pid" 2>/dev/null; do
        local r h
        r=$(rss_kb "$pid")
        if [ -n "$r" ]; then
            sum=$((sum + r)); n=$((n + 1))
            [ "$r" -gt "$peak" ] && peak=$r
        fi
        h=$(hwm_kb "$pid")
        [ -n "$h" ] && [ "$h" -gt "$peak" ] && peak=$h
        local tk
        tk=$(cpu_ticks "$pid")
        # NOTE: an idle client can legitimately report 0 CPU ticks; always record.
        [ -n "$tk" ] && last_ticks=$tk && last_wall=$(date +%s%N)
        sleep 0.25
    done
    wait "$pid" 2>/dev/null
    local client_rc=$?

    kill "$mock_pid" 2>/dev/null
    wait "$mock_pid" 2>/dev/null

    local avg=$((n > 0 ? sum / n : 0))
    local dt_ticks=$((last_ticks - settle_ticks))
    local dt_ms=$(((last_wall - settle_wall) / 1000000))
    local cpu_pct="?"
    if [ "$dt_ms" -gt 0 ]; then
        cpu_pct=$(awk -v t="$dt_ticks" -v d="$dt_ms" -v c="$CLK_TCK" \
            'BEGIN { printf "%.1f", 100.0 * t / c / (d / 1000.0) }')
    fi
    local b2m
    b2m=$(sed -n 's/.*bind->map \([0-9]*\)ms.*/\1/p' "$mock_log" | head -1)
    [ -z "$b2m" ] && b2m="?"

    echo "$name|${avg}|${peak}|${cpu_pct}|${dt_ms}|${b2m}|${client_rc}|n=${n}"
}

strip_size() {
    local f="$1"
    local tmp; tmp=$(mktemp)
    cp "$f" "$tmp" && strip "$tmp" 2>/dev/null
    stat -c %s "$tmp"
    rm -f "$tmp"
}

emit() { printf '%s\n' "$*" | tee -a "$RESULTS/measurements.raw"; }

: >"$RESULTS/measurements.raw"

echo "== Phase 2 stack spike measurements ($(date -Is)) ==" | tee "$RESULTS/measurements.md"
echo "" | tee -a "$RESULTS/measurements.md"

# --- runtime scenarios ---
emit "scenario|avg_rss_kb|peak_rss_kb|cpu_pct_after_settle|sampled_ms|bind_to_map_ms|rc"
for spike in spike-cpu spike-slint; do
    bin="$SPIKE_DIR/$spike/target/release/$spike"
    [ -x "$bin" ] || { emit "$spike-idle|SKIP (not built)"; continue; }
    run_scenario "$spike-idle" "$bin" idle 10
    run_scenario "$spike-anim" "$bin" animate 10 --animate
done

# --- binary sizes ---
emit "binary|raw_bytes|stripped_bytes"
for name in mock-compositor spike-cpu spike-gpu spike-slint; do
    bin="$SPIKE_DIR/$name/target/release/$name"
    [ -x "$bin" ] || { emit "$name|SKIP|SKIP"; continue; }
    raw=$(stat -c %s "$bin")
    stripped=$(strip_size "$bin")
    emit "$name|$raw|$stripped"
done

# --- line counts (layout code we must write) ---
emit "loc|main_rs|slint_markup"
main_rs=$(wc -l <"$SPIKE_DIR/spike-cpu/src/main.rs")
emit "spike-cpu|$main_rs|0"
main_rs=$(wc -l <"$SPIKE_DIR/spike-slint/src/main.rs")
markup=$(awk '/slint::slint! \{/,/^\}/' "$SPIKE_DIR/spike-slint/src/main.rs" | wc -l)
emit "spike-slint|$main_rs|$markup"
main_rs=$(wc -l <"$SPIKE_DIR/spike-gpu/src/main.rs")
emit "spike-gpu|$main_rs|0"

# --- clean build timing (destructive; opt-in) ---
if [ "${MEASURE_BUILDS:-0}" = "1" ]; then
    emit "build|seconds"
    export PATH="$HOME/.cargo/bin:$PATH"
    export PKG_CONFIG_PATH="${PKG_CONFIG_PATH:-}"
    export RUSTFLAGS="${RUSTFLAGS:-}"
    for d in mock-compositor spike-cpu spike-gpu spike-slint; do
        cd "$SPIKE_DIR/$d" || continue
        cargo clean -q
        local_start=$(date +%s)
        cargo build --release -q
        rc=$?
        local_end=$(date +%s)
        if [ $rc -eq 0 ]; then
            emit "$d|$((local_end - local_start))"
        else
            emit "$d|FAILED"
        fi
    done
fi

# render raw table as markdown
{
    echo ""
    echo "## Raw table"
    echo '```'
    cat "$RESULTS/measurements.raw"
    echo '```'
} >>"$RESULTS/measurements.md"

echo "Wrote $RESULTS/measurements.md"
