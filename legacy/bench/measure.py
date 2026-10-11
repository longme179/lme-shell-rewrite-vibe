#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Reproducible backend measurements for the rewrite baseline (Phase 0).

Measures, without touching system state:
  1. --once startup wall time and child max RSS (tools absent vs stub tools)
  2. steady-state RSS of the running backend over N seconds (open stdin)
  3. subprocess invocations per poll cycle (stub tools log every call)
  4. snapshot JSON line size with/without the apps field
  5. clipboard service steady-state RSS

Usage: python3 bench/measure.py [--backend python|rust] [--seconds 11]
Writes a human-readable report to stdout. Requires python3 only.
"""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import subprocess
import sys
import time

REPO = Path(__file__).resolve().parents[1]
BENCH = REPO / "bench"
STUBS = BENCH / "stubs"


def stub_env(log: Path) -> dict[str, str]:
    env = dict(os.environ)
    env["PATH"] = f"{STUBS}:{env['PATH']}"
    env["LME_STUB_LOG"] = str(log)
    env["LC_ALL"] = "C"
    return env


def plain_env() -> dict[str, str]:
    env = dict(os.environ)
    env["LC_ALL"] = "C"
    return env


def once_timing(backend: str, env: dict[str, str]) -> tuple[list[str], list[str]]:
    command = (["python3", str(REPO / "scripts/desktop_backend.py")]
               if backend == "python" else [backend])
    lines: list[str] = []
    calls: list[str] = []
    for _ in range(3):
        log = Path(env.get("LME_STUB_LOG", "/tmp/lme-stub-calls.log"))
        if env.get("LME_STUB_LOG") and log.exists():
            log.unlink()
        start = time.monotonic()
        proc = subprocess.run(command + ["--once"], env=env,
                              capture_output=True, text=True)
        wall = time.monotonic() - start
        lines.append(f"wall={wall:.3f}s snapshot={len(proc.stdout)}B exit={proc.returncode}")
        if env.get("LME_STUB_LOG") and log.exists():
            calls = log.read_text().splitlines()
    return lines, calls


def rss_of(pid: int) -> int | None:
    try:
        for line in Path(f"/proc/{pid}/status").read_text().splitlines():
            if line.startswith("VmRSS:"):
                return int(line.split()[1])
    except OSError:
        pass
    return None


def steady_state(command: list[str], env: dict[str, str], seconds: float,
                 log: Path | None = None, interval: str | None = "2.0") -> tuple[int, int]:
    """Run with stdin held open (PIPE); sample RSS; return (peak kB, subprocess calls)."""
    if log and log.exists():
        log.unlink()
    argv = command + (["--interval", interval] if interval else [])
    proc = subprocess.Popen(argv, env=env,
                            stdin=subprocess.PIPE, stdout=subprocess.DEVNULL,
                            stderr=subprocess.DEVNULL)
    peak = 0
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if (rss := rss_of(proc.pid)) is None:
            break
        peak = max(peak, rss)
        time.sleep(0.5)
    proc.terminate()
    try:
        proc.wait(timeout=3)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait()
    calls = len(log.read_text().splitlines()) if log and log.exists() else 0
    return peak, calls


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--backend", default="python", choices=["python", "rust"],
                        help="python runs scripts/desktop_backend.py; rust takes the binary path from $LME_RUST_BACKEND")
    parser.add_argument("--seconds", type=float, default=11.0)
    args = parser.parse_args()

    if args.backend == "python":
        command = ["python3", str(REPO / "scripts/desktop_backend.py")]
        clipboard = ["python3", str(REPO / "scripts/clipboard.py"), "--directory", "/tmp/lme-measure-clip"]
    else:
        binary = os.environ.get("LME_RUST_BACKEND")
        if not binary:
            print("Set LME_RUST_BACKEND to the lme-backend binary path.", file=sys.stderr)
            return 2
        command = [binary]
        clipboard = [binary, "--clipboard", "--directory", "/tmp/lme-measure-clip"]

    print(f"== backend: {args.backend} ==")
    log = Path("/tmp/lme-measure-calls.log")
    print("-- once, no desktop tools on PATH --")
    lines, _ = once_timing("python" if args.backend == "python" else os.environ["LME_RUST_BACKEND"], plain_env())
    for line in lines:
        print(f"   {line}")
    print("-- once, stub tools present (max probe churn) --")
    lines, calls = once_timing("python" if args.backend == "python" else os.environ["LME_RUST_BACKEND"], stub_env(log))
    for line in lines:
        print(f"   {line}")
    per_snapshot = [c for c in calls if "event-stream" not in c]
    print(f"   subprocesses per snapshot: {len(per_snapshot)}")
    for line in sorted(set(per_snapshot)):
        print(f"     {line}")

    print(f"-- steady state {args.seconds:.0f}s, interval 2.0 --")
    peak, calls = steady_state(command, stub_env(log), args.seconds, log)
    print(f"   peak RSS: {peak} kB")
    if log.exists():
        lines = [c for c in log.read_text().splitlines() if "event-stream" not in c]
        persistent = sum(1 for c in log.read_text().splitlines() if "event-stream" in c)
        print(f"   subprocess calls total: {len(lines)} (+{persistent} persistent event-stream)")
    print("-- clipboard service steady state --")
    cpeak, _ = steady_state(clipboard, stub_env(Path("/tmp/lme-unused.log")), args.seconds, interval=None)
    print(f"   peak RSS: {cpeak} kB")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
