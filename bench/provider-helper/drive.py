#!/usr/bin/env python3
"""Drive and measure the EXPERIMENTAL provider-helper mode over its Unix socket.

    python3 -I bench/provider-helper/drive.py --kwiry daemon/target/release/kwiry \
        --corpus <corpus-dir> --work <scratch-dir> [--connections 1]

<corpus-dir> holds vault/ and manifest.tsv (bench/scale-62k/generate.mjs). It
is only read: ids and versions are derived here from paths and bytes, and
edits exist only in what is pushed to the helper. <scratch-dir> must not
exist; it receives the helper's socket directory, its disposable data
directory, the /usr/bin/time -v log and results.json.

Wire: the search provider contract (DRAFT v1) section 2.3. Sequence: hello;
begin(epoch 1, full); one PUT per note (keep-alive, raw bytes) with a
checkpoint every 1,000 notes; commit; index size; ~50 queries x 3 passes
(the bench/scale-62k query set, limit 20); five one-edit updates (delta begin,
put, checkpoint, commit, until a query shows the edit); five deletes (same,
until a query stops showing it); status; retire; SIGTERM and peak RSS from
/usr/bin/time -v. Also checks the helper holds no TCP/UDP sockets.

Aborts (and kills the helper) if MemAvailable drops under 4 GiB.
"""

import argparse
import hashlib
import http.client
import importlib.util
import json
import os
import re
import signal
import socket
import subprocess
import sys
import threading
import time
from pathlib import Path
from urllib.parse import quote

HERE = Path(__file__).resolve().parent
MIN_AVAILABLE_KIB = 4 * 1024 * 1024
PARTITION = "trial-workspace"
LIMIT = 20


def load_measure():
    """The query set and percentile helpers of the earlier scale run, so the
    figures are comparable with bench/scale-62k results."""
    spec = importlib.util.spec_from_file_location(
        "scale_measure", HERE.parent / "scale-62k" / "measure.py"
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def log(message):
    print(f"[{time.strftime('%H:%M:%S')}] {message}", flush=True)


def mem_available_kib():
    with open("/proc/meminfo", encoding="ascii") as handle:
        for line in handle:
            if line.startswith("MemAvailable:"):
                return int(line.split()[1])
    return 0


class UnixConnection(http.client.HTTPConnection):
    def __init__(self, path, timeout=60):
        super().__init__("localhost", timeout=timeout)
        self.unix_path = path

    def connect(self):
        sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        sock.settimeout(self.timeout)
        sock.connect(self.unix_path)
        self.sock = sock


class Client:
    """One keep-alive connection."""

    def __init__(self, path):
        self.connection = UnixConnection(path)

    def call(self, method, target, body=None, headers=None):
        headers = dict(headers or {})
        headers["Search-Provider-Profile"] = "v1"
        if isinstance(body, (dict, list)):
            body = json.dumps(body).encode()
            headers["Content-Type"] = "application/json"
        self.connection.request(method, target, body=body, headers=headers)
        response = self.connection.getresponse()
        raw = response.read()
        return response.status, (json.loads(raw) if raw else None)

    def put(self, partition, item_id, path, data, version=None):
        return self.call(
            "PUT",
            f"/v1/partitions/{quote(partition, safe='')}/items/{quote(item_id, safe='')}",
            body=data,
            headers={
                "Content-Type": "application/octet-stream",
                "Provider-Item-Version": version or hashlib.sha256(data).hexdigest(),
                "Provider-Item-Path": quote(path, safe="/"),
            },
        )

    def post(self, partition, verb, body):
        return self.call("POST", f"/v1/partitions/{quote(partition, safe='')}/{verb}", body)

    def delete(self, partition, item_id):
        return self.call(
            "DELETE",
            f"/v1/partitions/{quote(partition, safe='')}/items/{quote(item_id, safe='')}",
        )

    def query(self, text, limit=LIMIT):
        return self.call(
            "POST",
            "/v1/query",
            {"partitions": [PARTITION], "q": {"text": text, "mode": "lexical"}, "terms": [],
             "limit": limit, "deadlineMs": int(time.time() * 1000) + 30000},
        )


def item_id(path):
    """Opaque, stable identity derived from the path (the host's choice)."""
    return hashlib.sha256(b"note:" + path.encode()).hexdigest()[:32]


def du_bytes(path):
    total = 0
    for root, _, files in os.walk(path):
        for name in files:
            try:
                total += os.lstat(os.path.join(root, name)).st_size
            except FileNotFoundError:
                pass
    return total


def inet_socket_inodes():
    inodes = set()
    for table in ("tcp", "tcp6", "udp", "udp6"):
        try:
            with open(f"/proc/net/{table}", encoding="ascii") as handle:
                next(handle)
                for line in handle:
                    inodes.add(line.split()[9])
        except FileNotFoundError:
            pass
    return inodes


def helper_pid(time_pid):
    children = Path(f"/proc/{time_pid}/task/{time_pid}/children").read_text().split()
    return int(children[0])


def socket_check(pid):
    inodes = set()
    for fd in Path(f"/proc/{pid}/fd").iterdir():
        try:
            target = os.readlink(fd)
        except OSError:
            continue
        match = re.fullmatch(r"socket:\[(\d+)\]", target)
        if match:
            inodes.add(match.group(1))
    inet = inodes & inet_socket_inodes()
    return {"sockets": len(inodes), "inet_sockets": len(inet)}


def wait_until(predicate, timeout, interval=0.005):
    start = time.perf_counter()
    while time.perf_counter() - start < timeout:
        if predicate():
            return time.perf_counter() - start
        time.sleep(interval)
    return None


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--kwiry", required=True, type=Path)
    parser.add_argument("--corpus", required=True, type=Path)
    parser.add_argument("--work", required=True, type=Path)
    parser.add_argument("--connections", type=int, default=1)
    args = parser.parse_args()
    measure = load_measure()

    vault = args.corpus / "vault"
    paths = measure.manifest_paths(args.corpus)
    queries = measure.derive_queries(vault, paths)
    args.work.mkdir(parents=True)
    run_dir = args.work / "run"
    run_dir.mkdir(mode=0o700)
    data_dir = args.work / "data"
    sock_path = str(run_dir / "provider.sock")
    time_log = args.work / "time-v.log"
    results = {
        "corpus": str(args.corpus),
        "notes": len(paths),
        "connections": args.connections,
        "kwiry": str(args.kwiry),
    }

    helper = subprocess.Popen(
        ["/usr/bin/time", "-v", "-o", str(time_log), str(args.kwiry),
         "--data-dir", str(data_dir), "provider-helper", "--socket", sock_path],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    stop = threading.Event()

    def guard():
        while not stop.wait(0.5):
            if mem_available_kib() < MIN_AVAILABLE_KIB:
                log("MemAvailable under 4 GiB: killing the helper")
                helper.send_signal(signal.SIGKILL)
                os._exit(3)

    threading.Thread(target=guard, daemon=True).start()
    try:
        if wait_until(lambda: os.path.exists(sock_path), 30, 0.02) is None:
            raise SystemExit("helper did not start")
        client = Client(sock_path)
        status, hello = client.call("GET", "/v1/hello")
        assert status == 200, hello
        results["hello"] = hello
        log(f"hello: {hello['name']} {hello['version']} build={hello.get('build')}")

        # Read the corpus first so the feed timing is the helper's, not the disk's.
        notes = [(path, (vault / path).read_bytes()) for path in paths]
        corpus_bytes = sum(len(data) for _, data in notes)

        status, body = client.post(PARTITION, "begin", {"epoch": 1, "mode": "full"})
        assert status == 200, body
        dispositions = {}
        lock = threading.Lock()
        started = time.perf_counter()

        def feed(chunk):
            own = Client(sock_path)
            for path, data in chunk:
                status, body = own.put(PARTITION, item_id(path), path, data)
                assert status == 200, (path, status, body)
                with lock:
                    dispositions["accepted"] = dispositions.get("accepted", 0) + 1

        n = 0
        for start in range(0, len(notes), 1000):
            batch = notes[start : start + 1000]
            workers = [
                threading.Thread(target=feed, args=(batch[i :: args.connections],))
                for i in range(args.connections)
            ]
            for worker in workers:
                worker.start()
            for worker in workers:
                worker.join()
            n += 1
            status, body = client.post(PARTITION, "checkpoint", {"n": n})
            assert status == 200, body
        puts_done = time.perf_counter()
        status, body = client.post(PARTITION, "commit", {})
        assert status == 200 and body["asOf"] == {"epoch": 1, "n": n}, body
        committed = time.perf_counter()
        results["feed"] = {
            "corpus_bytes": corpus_bytes,
            "puts_s": round(puts_done - started, 3),
            "commit_s": round(committed - puts_done, 3),
            "total_s": round(committed - started, 3),
            "dispositions": dispositions,
        }
        log(f"feed: {results['feed']}")
        results["index_bytes_after_commit"] = du_bytes(data_dir)
        pid = helper_pid(helper.pid)
        results["socket_check"] = socket_check(pid)
        log(f"index {results['index_bytes_after_commit'] / 2**20:.1f} MiB; sockets {results['socket_check']}")

        # Query latency: ~50 queries x 3 passes, timed around the exchange.
        per_query, samples = [], []
        for _ in range(3):
            for kind, q in queries:
                begin = time.perf_counter()
                status, body = client.query(q)
                elapsed = (time.perf_counter() - begin) * 1000
                assert status == 200, (q, body)
                assert body["coverage"][0]["state"] == "ready"
                samples.append(elapsed)
                per_query.append({"kind": kind, "q": q, "ms": round(elapsed, 2),
                                  "hits": len(body["refs"])})
        results["query"] = measure.summarize(samples)
        results["zero_hit_queries"] = sum(1 for r in per_query[: len(queries)] if r["hits"] == 0)
        results["queries"] = per_query
        log(f"query: {results['query']}")

        # One-edit update and delete visibility, five times each.
        edits, deletes = [], []
        for index, (path, data) in enumerate(notes[:: max(1, len(notes) // 5)][:5]):
            marker = f"zqprovidermarker{index}x"
            edited = data + f"\n\n{marker}\n".encode()
            ident = item_id(path)
            n += 1
            begin = time.perf_counter()
            status, body = client.post(PARTITION, "begin", {"epoch": 1, "mode": "delta"})
            assert status == 200, body
            status, body = client.put(PARTITION, ident, path, edited)
            assert status == 200, body
            status, body = client.post(PARTITION, "checkpoint", {"n": n})
            assert status == 200, body
            status, body = client.post(PARTITION, "commit", {})
            assert status == 200 and body["asOf"] == {"epoch": 1, "n": n}, body
            checkpointed = time.perf_counter() - begin

            def shows(expect):
                status, body = client.query(marker, 5)
                found = any(r["id"] == ident for r in body["refs"])
                return found == expect and body["coverage"][0]["asOf"]["n"] == n

            visible = wait_until(lambda: shows(True), 60)
            assert visible is not None, f"edit {marker} never became visible"
            edits.append({"feed_commit_ms": round(checkpointed * 1000, 2),
                          "visible_ms": round((time.perf_counter() - begin) * 1000, 2)})

            n += 1
            begin = time.perf_counter()
            status, body = client.post(PARTITION, "begin", {"epoch": 1, "mode": "delta"})
            assert status == 200, body
            status, body = client.delete(PARTITION, ident)
            assert status == 200, body
            status, body = client.post(PARTITION, "checkpoint", {"n": n})
            assert status == 200, body
            status, body = client.post(PARTITION, "commit", {})
            assert status == 200, body
            checkpointed = time.perf_counter() - begin
            gone = wait_until(lambda: shows(False), 60)
            assert gone is not None, f"delete of {marker} never became visible"
            deletes.append({"feed_commit_ms": round(checkpointed * 1000, 2),
                            "visible_ms": round((time.perf_counter() - begin) * 1000, 2)})
        results["one_edit"] = edits
        results["delete"] = deletes
        log(f"one-edit: {edits}")
        log(f"delete: {deletes}")

        # Version mismatch is refused.
        client.post(PARTITION, "begin", {"epoch": 1, "mode": "delta"})
        status, body = client.put(PARTITION, "mismatch", "mismatch.md", b"x", "0" * 64)
        assert status == 422 and body["error"]["code"] == "version_mismatch", body
        n += 1
        client.post(PARTITION, "checkpoint", {"n": n})
        client.post(PARTITION, "commit", {})

        status, body = client.call("GET", f"/v1/partitions/{PARTITION}/status")
        assert status == 200, body
        results["status"] = body
        log(f"status: {body}")
        results["index_bytes_end"] = du_bytes(data_dir)
        status, body = client.call("DELETE", f"/v1/partitions/{PARTITION}")
        assert status == 200, body
        results["index_bytes_after_retire"] = du_bytes(data_dir)
        hwm = re.search(r"VmHWM:\s+(\d+) kB", Path(f"/proc/{pid}/status").read_text())
        results["vmhwm_kib"] = int(hwm.group(1)) if hwm else None
    finally:
        stop.set()
        # Signal the helper itself: /usr/bin/time writes its report only when
        # its child exits, and dies silently if it is the one signalled.
        try:
            os.kill(helper_pid(helper.pid), signal.SIGTERM)
        except (OSError, IndexError, ValueError):
            helper.send_signal(signal.SIGTERM)
        try:
            helper.wait(timeout=30)
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
