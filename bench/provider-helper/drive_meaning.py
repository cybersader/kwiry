#!/usr/bin/env python3
"""Measure the EXPERIMENTAL provider helper's lexical, semantic and hybrid kinds.

    python3 -I bench/provider-helper/drive_meaning.py --kwiry daemon/target/release/kwiry \
        --corpus <corpus-dir> --work <scratch-dir> --model-dir <pinned-model-dir> \
        [--max-notes N] [--embed-threads N]

<corpus-dir> holds vault/ and manifest.tsv (bench/scale-62k/generate.mjs); it is
only read. --max-notes feeds the first N manifest paths (a subset). <scratch-dir>
must not exist; it receives the socket directory, the disposable data
directory, the /usr/bin/time -v log and results.json.

Sequence: hello (must declare semantic and hybrid); begin(full); one PUT per
note with a checkpoint every 1,000; commit; time until the lexical kind is
ready, then until the semantic kind is ready at the same asOf (the background
embedding time), sampling the helper's RSS; index size; the bench/scale-62k
query set x 3 in each of lexical, semantic and hybrid (limit 20); five edits,
each timed until visible per kind (lexical: the marker word is found;
semantic: the semantic kind is ready at the edit's commit and a semantic query
for the added sentence returns the note at its new version); status; retire;
SIGTERM and peak RSS from /usr/bin/time -v.

Aborts (and kills the helper) if MemAvailable drops under 4 GiB.
"""

import argparse
import importlib.util
import json
import os
import re
import signal
import subprocess
import sys
import threading
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
LIMIT = 20
PARTITION = "trial-workspace"


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


drive = load("provider_drive", HERE / "drive.py")
measure = drive.load_measure()
log = drive.log


def ask(client, text, mode, limit=LIMIT):
    status, body = client.call(
        "POST",
        "/v1/query",
        {"partitions": [PARTITION], "q": {"text": text, "mode": mode}, "terms": [],
         "limit": limit, "deadlineMs": int(time.time() * 1000) + 30000},
    )
    assert status == 200, (text, mode, body)
    return body


def coverage(body, kind):
    return next(c for c in body["coverage"] if c["mode"] == kind)


def status_of(client):
    status, body = client.call("GET", f"/v1/partitions/{PARTITION}/status")
    assert status == 200, body
    return body


def rss_kib(pid):
    match = re.search(r"VmRSS:\s+(\d+) kB", Path(f"/proc/{pid}/status").read_text())
    return int(match.group(1)) if match else 0


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--kwiry", required=True, type=Path)
    parser.add_argument("--corpus", required=True, type=Path)
    parser.add_argument("--work", required=True, type=Path)
    parser.add_argument("--model-dir", required=True, type=Path)
    parser.add_argument("--max-notes", type=int)
    parser.add_argument("--embed-threads", type=int)
    parser.add_argument("--embed-timeout", type=int, default=7200)
    args = parser.parse_args()

    vault = args.corpus / "vault"
    paths = measure.manifest_paths(args.corpus)
    if args.max_notes:
        paths = paths[: args.max_notes]
    queries = measure.derive_queries(vault, paths)
    args.work.mkdir(parents=True)
    run_dir = args.work / "run"
    run_dir.mkdir(mode=0o700)
    data_dir = args.work / "data"
    sock_path = str(run_dir / "provider.sock")
    time_log = args.work / "time-v.log"
    results = {"corpus": str(args.corpus), "notes": len(paths), "kwiry": str(args.kwiry),
               "embed_threads": args.embed_threads}

    command = [str(args.kwiry), "--data-dir", str(data_dir), "provider-helper",
               "--socket", sock_path, "--model-dir", str(args.model_dir)]
    if args.embed_threads:
        command += ["--embed-threads", str(args.embed_threads)]
    helper = subprocess.Popen(["/usr/bin/time", "-v", "-o", str(time_log), *command],
                              stdout=subprocess.DEVNULL, stderr=open(args.work / "helper.err", "wb"))
    stop = threading.Event()

    def guard():
        while not stop.wait(0.5):
            if drive.mem_available_kib() < drive.MIN_AVAILABLE_KIB:
                log("MemAvailable under 4 GiB: killing the helper")
                helper.send_signal(signal.SIGKILL)
                os._exit(3)

    threading.Thread(target=guard, daemon=True).start()
    try:
        if drive.wait_until(lambda: os.path.exists(sock_path), 120, 0.05) is None:
            raise SystemExit("helper did not start")
        pid = drive.helper_pid(helper.pid)
        client = drive.Client(sock_path)
        status, hello = client.call("GET", "/v1/hello")
        assert status == 200, hello
        assert hello["capabilities"]["retrieval"] == ["lexical", "semantic", "hybrid"], hello
        results["hello"] = hello
        results["rss_after_start_kib"] = rss_kib(pid)
        log(f"hello: {hello['capabilities']}; RSS {results['rss_after_start_kib']} KiB")

        notes = [(path, (vault / path).read_bytes()) for path in paths]
        status, body = client.post(PARTITION, "begin", {"epoch": 1, "mode": "full"})
        assert status == 200, body
        started = time.perf_counter()
        n = 0
        for start in range(0, len(notes), 1000):
            for path, data in notes[start:start + 1000]:
                status, body = client.put(PARTITION, drive.item_id(path), path, data)
                assert status == 200, (path, status, body)
            n += 1
            status, body = client.post(PARTITION, "checkpoint", {"n": n})
            assert status == 200, body
        status, body = client.post(PARTITION, "commit", {})
        assert status == 200 and body["asOf"] == {"epoch": 1, "n": n}, body
        lexical_ready = time.perf_counter() - started
        log(f"lexical committed in {lexical_ready:.1f} s")

        samples = []
        peak_sample = 0
        last_log = time.perf_counter()
        while True:
            st = status_of(client)
            sem = st["semantic"]
            rss = rss_kib(pid)
            peak_sample = max(peak_sample, rss)
            elapsed = time.perf_counter() - started
            if sem["state"] == "ready" and sem["asOf"] == st["asOf"]:
                break
            assert sem["state"] != "unavailable", st
            if elapsed > args.embed_timeout:
                raise SystemExit(f"embedding did not finish in {args.embed_timeout} s")
            if time.perf_counter() - last_log > 30:
                samples.append({"s": round(elapsed, 1), "rss_kib": rss})
                log(f"embedding... {elapsed:.0f} s, RSS {rss} KiB")
                last_log = time.perf_counter()
            time.sleep(0.25)
        semantic_ready = time.perf_counter() - started
        results["feed"] = {
            "corpus_bytes": sum(len(d) for _, d in notes),
            "lexical_ready_s": round(lexical_ready, 2),
            "semantic_ready_s": round(semantic_ready, 2),
            "embedding_s": round(semantic_ready - lexical_ready, 2),
            "rss_samples": samples,
            "sampled_peak_rss_kib": peak_sample,
        }
        log(f"semantic ready after {semantic_ready:.1f} s")
        results["status_after_build"] = status_of(client)
        results["index_bytes_after_build"] = drive.du_bytes(data_dir)
        results["socket_check"] = drive.socket_check(pid)

        per_kind = {}
        for mode in ("lexical", "semantic", "hybrid"):
            timings, zero = [], 0
            for rep in range(3):
                for kind, q in queries:
                    begin = time.perf_counter()
                    body = ask(client, q, mode)
                    timings.append((time.perf_counter() - begin) * 1000)
                    if rep == 0 and not body["refs"]:
                        zero += 1
                    for entry in body["coverage"]:
                        assert entry["state"] == "ready", body["coverage"]
            per_kind[mode] = {**measure.summarize(timings), "zero_hit_queries": zero}
            log(f"{mode}: {per_kind[mode]}")
        results["query"] = per_kind

        edits = []
        for index, (path, data) in enumerate(notes[:: max(1, len(notes) // 5)][:5]):
            marker = f"zqmeaningmarker{index}x"
            sentence = "A violet heron balances seven paper lanterns above the frozen canal"
            # Its own heading section, so it becomes its own chunk (and vector),
            # as a real added paragraph under a new heading would.
            edited = data + f"\n\n## Violet heron\n\n{marker} {sentence}.\n".encode()
            version = drive.hashlib.sha256(edited).hexdigest()
            ident = drive.item_id(path)
            n += 1
            begin = time.perf_counter()
            client.post(PARTITION, "begin", {"epoch": 1, "mode": "delta"})
            status, body = client.put(PARTITION, ident, path, edited)
            assert status == 200, body
            client.post(PARTITION, "checkpoint", {"n": n})
            status, body = client.post(PARTITION, "commit", {})
            assert status == 200 and body["asOf"]["n"] == n, body
            committed = time.perf_counter() - begin

            def lexical_shows():
                body = ask(client, marker, "lexical", 5)
                return coverage(body, "lexical")["asOf"]["n"] == n and any(
                    r["id"] == ident and r["version"] == version for r in body["refs"])

            def semantic_shows():
                body = ask(client, sentence, "semantic", 200)
                cov = coverage(body, "semantic")
                return cov["state"] == "ready" and cov["asOf"]["n"] == n and any(
                    r["id"] == ident and r["version"] == version for r in body["refs"])

            lexical = drive.wait_until(lexical_shows, 60)
            lexical_ms = (time.perf_counter() - begin) * 1000
            semantic = drive.wait_until(semantic_shows, 120)
            semantic_ms = (time.perf_counter() - begin) * 1000
            assert lexical is not None and semantic is not None, (marker, lexical, semantic)
            edits.append({"feed_commit_ms": round(committed * 1000, 2),
                          "lexical_visible_ms": round(lexical_ms, 2),
                          "semantic_visible_ms": round(semantic_ms, 2)})
        results["one_edit"] = edits
        log(f"edits: {edits}")

        results["status"] = status_of(client)
        results["index_bytes_end"] = drive.du_bytes(data_dir)
        status, body = client.call("DELETE", f"/v1/partitions/{PARTITION}")
        assert status == 200, body
        hwm = re.search(r"VmHWM:\s+(\d+) kB", Path(f"/proc/{pid}/status").read_text())
        results["vmhwm_kib"] = int(hwm.group(1)) if hwm else None
    finally:
        stop.set()
        try:
            os.kill(drive.helper_pid(helper.pid), signal.SIGTERM)
        except (OSError, IndexError, ValueError):
            helper.send_signal(signal.SIGTERM)
        try:
            helper.wait(timeout=60)
        except subprocess.TimeoutExpired:
            helper.kill()
            helper.wait()
    rss = re.search(r"Maximum resident set size \(kbytes\): (\d+)", time_log.read_text())
    results["peak_rss_kib"] = int(rss.group(1)) if rss else None
    log(f"peak RSS {results['peak_rss_kib']} KiB")
    (args.work / "results.json").write_text(json.dumps(results, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
