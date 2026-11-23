#!/usr/bin/env python3
"""Reproduce the live-check-v1 stalled-reader case on a large governed room.

    python3 bench/scale-62k/stalled_reader.py --kwiry <release kwiry> \
        --work <measure.py scratch dir> --out <fixture.json>

Reuses the governed configuration and index that measure.py left in
<work>/governed. Mirrors daemon/crates/kwiry/tests/support/live_check_barrier.rs:
GDB non-stop mode on the unmodified binary, a breakpoint armed only for one
request, taken after that request has acquired its immutable labeled reader.
While request A is suspended, the room is relabeled and a fresh request B must
see the new label; then A is released and must still name the reader that
answered it. Also forces the ineligible-binding and global (invalid config)
refusals while A is suspended.

The test breaks on kwiry_core::runtime::search_opened_with_quality. That
function is inlined in a release build, so this script breaks on its only
callee, kwiry_core::search::search_partitions_with_quality, which receives the
already-opened partition readers. Same point in the request, same semantics.
"""

import argparse
import json
import subprocess
import sys
import threading
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from measure import Minter, capture, governed_body, wait_until  # noqa: E402

BREAK = "kwiry_core::search::search_partitions_with_quality"
LABEL_A = "5c" * 32
LABEL_B = "b7" * 32


class Barrier:
    def __init__(self, kwiry, config, data, root):
        self.root = root
        root.mkdir(parents=True, exist_ok=False)
        (root / "barrier.gdb").write_text(
            "set confirm off\n"
            "set pagination off\n"
            "set non-stop on\n"
            "set disable-randomization off\n"
            f"break {BREAK}\n"
            "disable 1\n"
            "commands 1\nsilent\ndisable 1\nshell touch pinned\nend\n"
            f"run --config '{config}' --data-dir '{data}' serve --bind 127.0.0.1:0 > daemon.out 2> daemon.err &\n"
        )
        self.gdb = subprocess.Popen(
            ["gdb", "-nx", "-nh", "-quiet", "-iex", "set auto-load off", "-iex",
             "set debuginfod enabled off", "-x", "barrier.gdb", "--args", str(kwiry)],
            cwd=root,
            stdin=subprocess.PIPE,
            stdout=open(root / "debug.out", "wb"),
            stderr=open(root / "debug.err", "wb"),
            text=True,
        )
        line = wait_until(lambda: self.listening(), 120, 0.1)
        if not line:
            raise SystemExit(f"daemon under gdb did not start: {(root / 'debug.err').read_text()[-2000:]}")
        address = line.split("http://", 1)[1].strip()
        self.host, port = address.rsplit(":", 1)
        self.port = int(port)

    def listening(self):
        out = self.root / "daemon.out"
        if out.exists():
            for line in out.read_text().splitlines():
                if "listening on http://" in line:
                    return line
        return None

    def cmd(self, text):
        self.gdb.stdin.write(text + "\n")
        self.gdb.stdin.flush()

    def wait_file(self, name):
        path = self.root / name
        if not wait_until(path.exists, 60, 0.02):
            raise SystemExit(f"barrier {name} not reached: {(self.root / 'debug.out').read_text()[-2000:]}")
        path.unlink()

    def close(self):
        try:
            self.cmd("quit")
            self.gdb.wait(timeout=30)
        except Exception:
            self.gdb.kill()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--kwiry", type=Path, required=True)
    parser.add_argument("--work", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--query", required=True)
    parser.add_argument("--timeout", type=float, default=900)
    args = parser.parse_args()
    work = args.work.resolve()
    config = work / "governed" / "control" / "config.toml"
    data = work / "governed" / "index-data"
    original = config.read_text()
    assert f'snapshot_id = "{LABEL_A}"' in original

    def write_config(text):
        staged = config.with_name("config.toml.staged")
        staged.write_text(text)
        staged.replace(config)

    def relabel(label):
        write_config(original.replace(LABEL_A, label))

    import http.client

    barrier = Barrier(args.kwiry.resolve(), config, data, work / "barrier")
    minter = Minter(work / "barrier")
    epoch = int((data / "replay-epoch").read_text())
    wait_until(lambda: int(time.time()) > epoch, 5)

    def request(q):
        headers = {"Authorization": f"Bearer {minter.mint()}", "Kwiry-Profile": "live-check-v1",
                   "Content-Type": "application/json"}
        body = governed_body(q)
        connection = http.client.HTTPConnection(barrier.host, barrier.port, timeout=900)
        start = time.perf_counter()
        connection.request("POST", "/v0/search", body=body, headers=headers)
        response = connection.getresponse()
        payload = response.read()
        elapsed = (time.perf_counter() - start) * 1000
        connection.close()
        return capture("", headers, body, (response.status, response.getheaders(), payload, elapsed))

    def label_of(reply):
        snapshots = reply["response"]["body"].get("resource_snapshots")
        return snapshots[0]["snapshot_id"] if snapshots else None

    def pause(q):
        barrier.cmd("enable 1\nshell touch armed")
        barrier.wait_file("armed")
        holder = {}
        thread = threading.Thread(target=lambda: holder.update(reply=request(q)))
        thread.start()
        barrier.wait_file("pinned")
        return thread, holder

    def release(thread, holder):
        barrier.cmd("continue -a&")
        thread.join(timeout=900)
        return holder["reply"]

    records = []
    notes = []

    def record(case, reply, status, observed):
        reply["case"] = case
        assert reply["response"]["status"] == status, (case, reply["response"]["status"], reply["response"]["body"])
        records.append(reply)
        notes.append({"case": case, **observed})

    try:
        ok = wait_until(lambda: label_of(request(args.query)) == LABEL_A, args.timeout, 0.5)
        if not ok:
            raise SystemExit("room never served label A")
        notes.append({"step": "served label A before pausing"})

        # 1. Hit query: A pinned, relabel to B, fresh B, release A -> still A.
        thread, holder = pause(args.query)
        start = time.monotonic()
        relabel(LABEL_B)
        fresh = wait_until(lambda: (r := request(args.query)) and label_of(r) == LABEL_B and r, args.timeout, 0.2)
        publish_s = round(time.monotonic() - start, 2)
        old = release(thread, holder)
        assert label_of(old) == LABEL_A and old["response"]["body"]["hits"], old
        record("label-only-hits", old, 200, {"stalled_label": label_of(old), "fresh_label_while_stalled": label_of(fresh),
                                              "fresh_hits": len(fresh["response"]["body"]["hits"]),
                                              "relabel_publish_seconds": publish_s})

        # 2. Zero-hit query: B pinned, relabel back to A, release -> still B, empty page.
        thread, holder = pause("zqxjvw")
        start = time.monotonic()
        relabel(LABEL_A)
        fresh = wait_until(lambda: (r := request("zqxjvw")) and label_of(r) == LABEL_A and r, args.timeout, 0.2)
        publish_s = round(time.monotonic() - start, 2)
        old = release(thread, holder)
        assert label_of(old) == LABEL_B and old["response"]["body"]["hits"] == [], old
        record("label-only-zero", old, 200, {"stalled_label": label_of(old), "fresh_label_while_stalled": label_of(fresh),
                                             "relabel_publish_seconds": publish_s})

        # 3. Ineligible binding published while A is pinned -> A refused with [0].
        thread, holder = pause(args.query)
        write_config(original.replace(LABEL_A, "generic-synthetic"))
        fresh = wait_until(lambda: (r := request(args.query))["response"]["body"].get("error", {}).get("resource_indexes") == [0] and r, args.timeout, 0.2)
        old = release(thread, holder)
        assert old["response"]["body"]["error"]["resource_indexes"] == [0], old
        record("final-ineligible-binding", old, 503, {"fresh_status_while_stalled": fresh["response"]["status"]})
        write_config(original)
        wait_until(lambda: label_of(request(args.query)) == LABEL_A, args.timeout, 0.2)

        # 4. Global refusal: invalid config while A is pinned -> A refused, no indexes.
        thread, holder = pause(args.query)
        config.write_text("[invalid")
        fresh = wait_until(lambda: (r := request(args.query))["response"]["status"] == 503 and r, args.timeout, 0.1)
        old = release(thread, holder)
        assert "resource_indexes" not in old["response"]["body"]["error"], old
        record("final-global-refusal", old, 503, {"fresh_status_while_stalled": fresh["response"]["status"]})
        write_config(original)
        wait_until(lambda: label_of(request(args.query)) == LABEL_A, args.timeout, 0.2)
    except (SystemExit, AssertionError, KeyError, TypeError) as error:
        notes.append({"failed": str(error)[:500]})
    finally:
        barrier.close()
        if config.read_text() != original:
            write_config(original)
        args.out.parent.mkdir(parents=True, exist_ok=True)
        args.out.write_text(json.dumps(records, indent=2, ensure_ascii=False) + "\n")
        args.out.with_suffix(".notes.json").write_text(json.dumps(notes, indent=2) + "\n")
        print(json.dumps(notes, indent=1))


if __name__ == "__main__":
    main()
