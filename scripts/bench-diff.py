#!/usr/bin/env python3
"""bench-diff — compare bench-results.json against a baseline.

Usage:
    bench-diff.py CURRENT BASELINE [--threshold 1.20] [--soak-threshold 1.20]

Exit code 0 if all metrics within threshold; 1 otherwise. Stdlib only.
"""
from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

NUMERIC_FIELDS = (
    "ns_per_iter",
    "allocs_per_iter",
    "p50_ms",
    "p99_ms",
    "rss_kb",
    "allocs_total",
    "payload_bytes",
)


def index(doc: dict) -> dict[tuple[str, str, str], float]:
    """Flatten doc.groups into {(group, name, field): value} for numeric fields only."""
    out: dict[tuple[str, str, str], float] = {}
    for group, items in doc.get("groups", {}).items():
        for item in items:
            name = item.get("name", "")
            for field in NUMERIC_FIELDS:
                v = item.get(field)
                if isinstance(v, (int, float)):
                    out[(group, name, field)] = float(v)
    return out


def main() -> int:
    p = argparse.ArgumentParser(description="Compare bench-results.json vs. baseline")
    p.add_argument("current", type=Path)
    p.add_argument("baseline", type=Path)
    p.add_argument("--threshold", type=float, default=1.20,
                   help="Default regression threshold (default 1.20x)")
    p.add_argument("--soak-threshold", type=float, default=1.20,
                   help="Override threshold for the soak group")
    args = p.parse_args()

    current = json.loads(args.current.read_text())
    baseline = json.loads(args.baseline.read_text())

    cur_idx = index(current)
    base_idx = index(baseline)

    failures: list[tuple[tuple[str, str, str], float, float, float]] = []
    for key, base_val in base_idx.items():
        cur_val = cur_idx.get(key)
        if cur_val is None:
            continue
        if base_val == 0:
            continue
        ratio = cur_val / base_val
        threshold = args.soak_threshold if key[0] == "soak" else args.threshold
        if ratio > threshold:
            failures.append((key, base_val, cur_val, ratio))

    if not failures:
        print("OK: no regressions over threshold")
        return 0

    print("REGRESSION:")
    print(f"  {'group/name/metric':70} {'baseline':>14} {'current':>14} {'ratio':>8}")
    for (group, name, field), base, cur, ratio in failures:
        label = f"{group}/{name}/{field}"
        print(f"  {label:70} {base:14.3f} {cur:14.3f} {ratio:8.2f}x")
    return 1


if __name__ == "__main__":
    sys.exit(main())
