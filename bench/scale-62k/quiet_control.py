#!/usr/bin/env python3
"""Control for governed_passes.py: no search traffic until after the first
60 s safety tick, then probe. Distinguishes engine behaviour from load.

    python3 bench/scale-62k/quiet_control.py --kwiry <bin> --work <scratch> [--quiet 75]
"""

import argparse
import json
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from governed_passes import proc_sample  # noqa: E402
from measure import Minter, Server, governed_body, wait_until  # noqa: E402


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--kwiry", type=Path, required=True)
    parser.add_argument("--work", type=Path, required=True)
    parser.add_argument("--quiet", type=float, default=75)
    args = parser.parse_args()
    work = args.work.resolve()
    config = work / "governed" / "control" / "config.toml"
    data = work / "governed" / "index-data"
    probe = work / "quiet-control"
    probe.mkdir()
    minter = Minter(probe)
    server = Server(args.kwiry.resolve(), config, data, "kwiry OpenClast sidecar listening on http://", probe, "quiet")
    epoch = int((data / "replay-epoch").read_text())
    wait_until(lambda: int(time.time()) > epoch, 5)

    def probe_once():
        h = {"Authorization": f"Bearer {minter.mint()}", "Kwiry-Profile": "live-check-v1", "Content-Type": "application/json"}
        status, _, payload, ms = server.request("POST", "/v0/search", h, governed_body("notes"))
        return {"t": round(time.monotonic() - server.started, 1), "status": status, "ms": round(ms, 1),
                "code": json.loads(payload).get("error", {}).get("code")}

    report = {"samples": [], "probes": []}
    try:
        # One probe once the boot reconcile has had time to settle, then silence.
        wait_until(lambda: probe_once()["status"] == 200, 120, 0.5)
        report["probes"].append(probe_once())
        while time.monotonic() - server.started < args.quiet:
            sample = proc_sample(server.process.pid)
            sample["t"] = round(time.monotonic() - server.started, 1)
            report["samples"].append(sample)
            time.sleep(5)
        for _ in range(10):
            report["probes"].append(probe_once())
            time.sleep(1)
        sample = proc_sample(server.process.pid)
        sample["t"] = round(time.monotonic() - server.started, 1)
        report["samples"].append(sample)
    finally:
        server.stop()
    (probe / "quiet.json").write_text(json.dumps(report, indent=1) + "\n")
    print(json.dumps(report, indent=1))


if __name__ == "__main__":
    main()
