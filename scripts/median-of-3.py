#!/usr/bin/env python3
"""Median-of-3 over three bench-results.json files.

For each (group, name, field) triple, take the median of the three values.
Pass through git_sha/timestamp/host from file 1.

Usage:
    median-of-3.py a.json b.json c.json > out.json
"""
from __future__ import annotations

import json
import statistics
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


def main() -> int:
    if len(sys.argv) != 4:
        print("usage: median-of-3 a.json b.json c.json > out.json", file=sys.stderr)
        return 2

    docs = [json.loads(Path(p).read_text()) for p in sys.argv[1:]]
    out: dict = {
        "schema": docs[0].get("schema", 1),
        "git_sha": docs[0].get("git_sha", ""),
        "timestamp": docs[0].get("timestamp", ""),
        "host": docs[0].get("host", {}),
        "groups": {},
    }

    groups: set[str] = set()
    for d in docs:
        groups.update(d.get("groups", {}).keys())

    for g in sorted(groups):
        # Zip by index — assumes same number of entries per file (true if
        # benches are deterministic, which they are).
        per_file = [d.get("groups", {}).get(g, []) for d in docs]
        n = min(len(x) for x in per_file)
        merged: list[dict] = []
        for i in range(n):
            base = dict(per_file[0][i])
            for f in NUMERIC_FIELDS:
                vals = [
                    a[i].get(f)
                    for a in per_file
                    if isinstance(a[i].get(f), (int, float))
                ]
                if len(vals) == 3:
                    base[f] = statistics.median(vals)
            merged.append(base)
        out["groups"][g] = merged

    print(json.dumps(out, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
