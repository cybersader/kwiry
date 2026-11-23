#!/usr/bin/env python3
"""Steady-state cost of the governed strict_hash safety pass, and repoint time.

    python3 bench/scale-62k/governed_passes.py --kwiry <release kwiry> \
        --corpus <corpus-dir> --work <measure.py scratch dir> \
        [--idle 200] [--out <json>]

Reuses the governed configuration and index measure.py left in <work>/governed.
Starts the governed daemon (profile "openclast", profile_required, strict_hash)
and, with no file changes, samples it every 0.1 s from /proc: CPU time
(utime+stime), bytes read (rchar: all read() bytes including page cache;
read_bytes: bytes fetched from storage), and RSS. A search thread sends one
live-check-v1 request every 0.2 s, rotating through the derived queries.

After --idle seconds the room's `path` is repointed to a fresh reflink copy of
the corpus (prepared before the clock starts) and the run continues until the
room serves 200 again from the new root, plus 20 s.

Safety passes are identified from the samples as bursts of read activity
(rchar growing by more than 1 MiB per 0.1 s sample, gaps under 1 s merged).
"""

import argparse
import json
import os
import subprocess
import sys
import threading
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from measure import (  # noqa: E402
    Minter, Server, derive_queries, governed_body, log, manifest_paths, percentile, wait_until,
)

CLK_TCK = os.sysconf("SC_CLK_TCK")


def proc_sample(pid):
    with open(f"/proc/{pid}/stat") as handle:
        fields = handle.read().rsplit(")", 1)[1].split()
    cpu = (int(fields[11]) + int(fields[12])) / CLK_TCK
    io = {}
    with open(f"/proc/{pid}/io") as handle:
        for line in handle:
            key, value = line.split(":")
            io[key] = int(value)
    rss = 0
    with open(f"/proc/{pid}/status") as handle:
        for line in handle:
            if line.startswith("VmRSS:"):
                rss = int(line.split()[1]) // 1024
    return {"cpu_s": cpu, "rchar": io["rchar"], "read_bytes": io["read_bytes"], "rss_mib": rss}


def find_passes(samples, threshold=1 << 20, merge_gap=1.0):
    bursts = []
    for prev, cur in zip(samples, samples[1:]):
        if cur["rchar"] - prev["rchar"] > threshold:
            if bursts and prev["t"] - bursts[-1][1]["t"] <= merge_gap:
                bursts[-1][1] = cur
            else:
                bursts.append([prev, cur])
    passes = []
    for start, end in bursts:
        window = [s for s in samples if start["t"] <= s["t"] <= end["t"]]
        passes.append({
            "start_s": round(start["t"], 2),
            "end_s": round(end["t"], 2),
            "wall_s": round(end["t"] - start["t"], 2),
            "cpu_s": round(end["cpu_s"] - start["cpu_s"], 2),
            "rchar_mib": round((end["rchar"] - start["rchar"]) / 2**20, 1),
            "storage_read_mib": round((end["read_bytes"] - start["read_bytes"]) / 2**20, 1),
            "peak_rss_mib": max(s["rss_mib"] for s in window),
        })
    return passes


def latency_split(queries, passes):
    inside, outside = [], []
    for q in queries:
        if q["status"] != 200:
            continue
        mid = q["t"] - q["ms"] / 2000
        target = inside if any(p["start_s"] <= mid <= p["end_s"] for p in passes) else outside
        target.append(q["ms"])

    def stats(values):
        if not values:
            return None
        return {"n": len(values), "p50_ms": round(percentile(values, 50), 1), "p95_ms": round(percentile(values, 95), 1), "max_ms": round(max(values), 1)}

    return {"during_passes": stats(inside), "outside_passes": stats(outside)}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--kwiry", type=Path, required=True)
    parser.add_argument("--corpus", type=Path, required=True)
    parser.add_argument("--work", type=Path, required=True)
    parser.add_argument("--idle", type=float, default=200)
    parser.add_argument("--repoint-timeout", type=float, default=1800)
    parser.add_argument("--out", type=Path)
    args = parser.parse_args()
    work = args.work.resolve()
    config = work / "governed" / "control" / "config.toml"
    data = work / "governed" / "index-data"
    original = config.read_text()
    old_root = work / "room"
    fresh_root = work / "room-fresh"
    assert json.dumps(str(old_root)) in original
    probe = work / "passes"
    probe.mkdir()
    log("preparing fresh reflink copy for the repoint step")
    subprocess.run(["cp", "-a", "--reflink=auto", str(args.corpus.resolve() / "vault"), str(fresh_root)], check=True)
    queries = [q for _, q in derive_queries(old_root, manifest_paths(args.corpus.resolve()))]

    minter = Minter(probe)
    server = Server(args.kwiry.resolve(), config, data, "kwiry OpenClast sidecar listening on http://", probe, "passes")
    epoch = int((data / "replay-epoch").read_text())
    wait_until(lambda: int(time.time()) > epoch, 5)
    t0 = server.started
    samples, results = [], []
    stop = threading.Event()

    def sampler():
        while not stop.is_set():
            try:
                sample = proc_sample(server.process.pid)
            except (FileNotFoundError, ProcessLookupError):
                return
            sample["t"] = time.monotonic() - t0
            samples.append(sample)
            time.sleep(0.1)

    def searcher():
        i = 0
        while not stop.is_set():
            q = queries[i % len(queries)]
            i += 1
            headers = {"Authorization": f"Bearer {minter.mint()}", "Kwiry-Profile": "live-check-v1", "Content-Type": "application/json"}
            status, response_headers, payload, ms = server.request("POST", "/v0/search", headers, governed_body(q))
            h = {k.lower(): v for k, v in response_headers}
            body = json.loads(payload)
            results.append({
                "t": round(time.monotonic() - t0, 3), "status": status, "ms": round(ms, 2),
                "freshness": h.get("x-kwiry-index-freshness"),
                "resource_indexes": body.get("error", {}).get("resource_indexes") if status != 200 else None,
                "code": body.get("error", {}).get("code") if status != 200 else None,
            })
            time.sleep(0.2)

    threads = [threading.Thread(target=sampler, daemon=True), threading.Thread(target=searcher, daemon=True)]
    for thread in threads:
        thread.start()
    report = {}
    try:
        log(f"idle observation for {args.idle:.0f} s")
        while time.monotonic() - t0 < args.idle:
            time.sleep(0.5)
        repoint_at = time.monotonic() - t0
        staged = config.with_name("config.toml.staged")
        staged.write_text(original.replace(json.dumps(str(old_root)), json.dumps(str(fresh_root))))
        staged.replace(config)
        log("repointed path to fresh copy")
        # Ready again = first 200 after the first non-200 that follows the repoint
        # (or the first 200 at all if the gate never closed).
        def recovered():
            after = [r for r in results if r["t"] >= repoint_at]
            seen_closed = next((i for i, r in enumerate(after) if r["status"] != 200), None)
            if seen_closed is None:
                return None
            return next((r for r in after[seen_closed:] if r["status"] == 200), None)
        back = wait_until(recovered, args.repoint_timeout, 0.2)
        report["repoint"] = {
            "at_s": round(repoint_at, 2),
            "first_unavailable_s": next((round(r["t"] - repoint_at, 2) for r in results if r["t"] >= repoint_at and r["status"] != 200), None),
            "serving_again_after_s": round(back["t"] - repoint_at, 2) if back else None,
            "freshness_when_back": back["freshness"] if back else None,
        }
        end = time.monotonic() + 20
        while time.monotonic() < end:
            time.sleep(0.5)
    finally:
        stop.set()
        for thread in threads:
            thread.join(timeout=10)
        server.stop()
        config.write_text(original)

    idle_samples = [s for s in samples if s["t"] < report.get("repoint", {}).get("at_s", 1e9)]
    passes = find_passes(idle_samples)
    report["idle_passes"] = passes
    report["idle_latency"] = latency_split([r for r in results if r["t"] < report["repoint"]["at_s"]], passes)
    report["idle_non_200"] = [r for r in results if r["status"] != 200 and r["t"] < report["repoint"]["at_s"]]
    repoint_samples = [s for s in samples if s["t"] >= report["repoint"]["at_s"]]
    if repoint_samples:
        first, last = repoint_samples[0], repoint_samples[-1]
        report["repoint"]["cpu_s_total_window"] = round(last["cpu_s"] - first["cpu_s"], 2)
        report["repoint"]["rchar_mib_window"] = round((last["rchar"] - first["rchar"]) / 2**20, 1)
        report["repoint"]["peak_rss_mib_window"] = max(s["rss_mib"] for s in repoint_samples)
    report["repoint"]["non_200_codes"] = sorted({(r["status"], r["code"], json.dumps(r["resource_indexes"])) for r in results if r["t"] >= report["repoint"]["at_s"] and r["status"] != 200})
    transitions, last = [], None
    for r in results:
        state = (r["status"], r["freshness"], r["code"])
        if state != last:
            transitions.append({"t": r["t"], "status": r["status"], "freshness": r["freshness"], "code": r["code"]})
            last = state
    report["status_transitions"] = transitions[:200]
    report["idle_status_counts"] = {
        "ok": sum(1 for r in results if r["status"] == 200 and r["t"] < report["repoint"]["at_s"]),
        "refused": len(report["idle_non_200"]),
    }
    report["idle_non_200"] = report["idle_non_200"][:20]
    report["peak_rss_mib_overall"] = max(s["rss_mib"] for s in samples)
    out = args.out or probe / "passes.json"
    out.write_text(json.dumps({"report": report, "samples": samples, "queries": results}, indent=1) + "\n")
    print(json.dumps(report, indent=1))


if __name__ == "__main__":
    main()
