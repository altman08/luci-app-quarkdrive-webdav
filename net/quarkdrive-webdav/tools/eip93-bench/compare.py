#!/usr/bin/env python3
"""Compare raw 0.2.0 Rust/Go samples; run on a development computer."""
import argparse
import csv
import math
import statistics


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("csv", nargs=2, help="Rust and Go raw CSV files, in either order")
    args = parser.parse_args()
    groups = {}
    implementations = set()
    for path in args.csv:
        with open(path, newline="", encoding="utf-8") as stream:
            rows = list(csv.DictReader(stream))
        if not rows:
            parser.error("no successful samples in " + path)
        impls = {r["implementation"] for r in rows}
        if len(impls) != 1 or not impls <= {"rust", "go"}:
            parser.error("each input must contain one implementation: " + path)
        impl = impls.pop()
        if impl in implementations:
            parser.error("need one Rust and one Go input")
        implementations.add(impl)
        for row in rows:
            if row["version"] != "0.2.0":
                parser.error("incompatible benchmark version in " + path)
            key = (int(row["key_bits"]), int(row["bytes"]), row["direction"], row["backend"])
            groups.setdefault(key, {}).setdefault(impl, []).append(row)
    print(" AES  bytes op backend    n(R/G)   Rust MiB/s   Go MiB/s   Go/Rust  CPU-ms/MiB(R/G)  machine%(R/G)")
    for key, by_impl in sorted(groups.items()):
        if set(by_impl) != {"rust", "go"}:
            print("MISSING peer samples:", key)
            continue
        r, g = by_impl["rust"], by_impl["go"]
        if key[3] != "software" and {v["driver"] for v in r} != {v["driver"] for v in g}:
            parser.error("kernel driver mismatch for " + str(key))

        def med(rows, column):
            return statistics.median(float(v[column]) for v in rows)

        rs, gs = med(r, "MiB_s"), med(g, "MiB_s")
        ratio = gs / rs if rs > 0 else math.nan
        print("%4d %6d %s %-9s %3d/%-3d %11.2f %10.2f %8.2fx %8.3f/%-8.3f %5.1f/%-5.1f" % (
            *key, len(r), len(g), rs, gs, ratio,
            med(r, "process_cpu_ms_per_MiB"), med(g, "process_cpu_ms_per_MiB"),
            med(r, "machine_busy_pct"), med(g, "machine_busy_pct")))
    print("Go/Rust > 1 means Go has higher throughput. CPU-ms/MiB: lower is better.")
    print("Run sequentially with matching --seconds/--repeats/--cpu; inspect stderr for failures.")


if __name__ == "__main__":
    main()
