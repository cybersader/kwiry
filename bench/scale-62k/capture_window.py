#!/usr/bin/env python3
"""Capture live-check-v1 success fixtures when the governed gate is only briefly open.

    python3 bench/scale-62k/capture_window.py --kwiry <bin> --work <scratch> \
        --query <hit word> --out <dir> [--attempts 6] [--window 40]

On this build a governed room whose full strict_hash pass opens more files than
the watcher channel holds (see RESULTS.md) closes its admission gate right after
boot reconciliation, sometimes before any request is answered. This script boots
the daemon at most --attempts times per case, polls every 20 ms for --window
seconds, and keeps the first 200 as the capture plus the first refusal after it.
Writes <out>/success.json (cases "hits", "no-hits", "gate-closed-after-pass")
and <out>/attempts.json.
"""

import argparse
import json
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from measure import Minter, Server, capture, governed_body, wait_until  # noqa: E402


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--kwiry", type=Path, required=True)
    parser.add_argument("--work", type=Path, required=True)
    parser.add_argument("--query", required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--attempts", type=int, default=6)
    parser.add_argument("--window", type=float, default=40)
    parser.add_argument("--cases", default="hits,no-hits")
    args = parser.parse_args()
    work = args.work.resolve()
    config = work / "governed" / "control" / "config.toml"
    data = work / "governed" / "index-data"
    probe = work / f"capture-window-{int(time.time())}"
    probe.mkdir()
    minter = Minter(probe)
    cases, attempts = [], []

    wanted = args.cases.split(",")
    for case, q in [(c, q) for c, q in [("hits", args.query), ("no-hits", "zqxjvw")] if c in wanted]:
        for attempt in range(args.attempts):
            server = Server(args.kwiry.resolve(), config, data, "kwiry OpenClast sidecar listening on http://", probe, f"{case}-{attempt}")
            record = {"case": case, "attempt": attempt, "ok_count": 0, "first_200_s": None, "last_200_s": None, "ok_ms": []}
            try:
                epoch = int((data / "replay-epoch").read_text())
                wait_until(lambda: int(time.time()) > epoch, 5)
                deadline = time.monotonic() + args.window
                captured = refusal = None
                while time.monotonic() < deadline:
                    h = {"Authorization": f"Bearer {minter.mint()}", "Kwiry-Profile": "live-check-v1", "Content-Type": "application/json"}
                    body = governed_body(q)
                    response = server.request("POST", "/v0/search", h, body)
                    t = round(time.monotonic() - server.started, 3)
                    if response[0] == 200:
                        record["ok_count"] += 1
                        record["ok_ms"].append(round(response[3], 2))
                        record["first_200_s"] = record["first_200_s"] or t
                        record["last_200_s"] = t
                        captured = captured or capture(case, h, body, response)
                    elif captured and not refusal:
                        refusal = capture("gate-closed-after-pass", h, body, response)
                        record["first_refusal_after_200_s"] = t
                        break
                    time.sleep(0.02)
            finally:
                server.stop()
            attempts.append(record)
            print(json.dumps({k: v for k, v in record.items() if k != "ok_ms"}), flush=True)
            if captured:
                cases.append(captured)
                if refusal and not any(c["case"] == "gate-closed-after-pass" for c in cases):
                    cases.append(refusal)
                break
    args.out.mkdir(parents=True, exist_ok=True)
    # Merge with earlier captures so separate runs per case accumulate.
    existing = args.out / "success.json"
    previous = json.loads(existing.read_text()) if existing.exists() else []
    merged = {c["case"]: c for c in previous}
    merged.update({c["case"]: c for c in cases})
    order = ["hits", "no-hits", "gate-closed-after-pass"]
    existing.write_text(json.dumps([merged[k] for k in order if k in merged], indent=2, ensure_ascii=False) + "\n")
    log_path = args.out / "attempts.json"
    history = json.loads(log_path.read_text()) if log_path.exists() else []
    log_path.write_text(json.dumps(history + attempts, indent=1) + "\n")
    print("captured", [c["case"] for c in cases])


if __name__ == "__main__":
    main()
