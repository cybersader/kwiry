#!/usr/bin/env python3
"""Scale measurements for the shipped kwiry binary on a generated corpus.

    python3 -I bench/scale-62k/measure.py --kwiry daemon/target/release/kwiry \
        --corpus <corpus-dir> --work <scratch-dir> [--fixtures <dir>]

<corpus-dir> is the output of generate.mjs (vault/ + manifest.tsv). The corpus
is never modified: the harness reflink-copies vault/ into <scratch-dir> and
mutates only that copy. <scratch-dir> must not exist yet.

Measured, lexical mode only:
  desktop  - full `kwiry index` (wall, peak RSS via /usr/bin/time -v, data size),
             incremental `kwiry index` after modifying 10, deleting 5 and
             renaming 5 notes, watcher time-to-searchable for 3 edits under
             `kwiry serve`, and HTTP search latency for ~50 queries x 3 passes.
  governed - the pilot configuration (profile "openclast", profile_required,
             loopback, strict_hash, workload-only auth, one room with a
             snapshot_id): full `kwiry index`, then live-check-v1 search latency
             with a fresh single-use capability per request.

Capabilities are signed with openssl using the synthetic Ed25519 test key that
the integration tests already embed (PRIVATE_DER in
daemon/crates/kwiry/tests/daemon.rs) and the same claim shape as
format_claims() there. No other key material is introduced. Latency is timed
around the HTTP exchange only; minting happens before the clock starts.

Aborts (and kills the child) if MemAvailable drops under 4 GiB while kwiry runs.
Results are written as JSON to <scratch-dir>/results.json.
"""

import argparse
import base64
import collections
import http.client
import json
import os
import re
import shutil
import signal
import statistics
import subprocess
import sys
import threading
import time
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
MIN_AVAILABLE_KIB = 4 * 1024 * 1024
TENANT = "tenant_AAAAAAAAAAAAAAAAAAAA"
VAULT_ID = "ScaleVault_0123456789a"
ROOM_ID = "ScaleRoom_0123456789ab"
SNAPSHOT_LABEL = "5c" * 32
JWKS = {
    "keys": [
        {
            "kty": "OKP",
            "crv": "Ed25519",
            "x": "2-Jj2UvNCvQiUPNYRgSi0cJSPiJI6Rs6D0UTeEpQVj8",
            "use": "sig",
            "key_ops": ["verify"],
            "alg": "EdDSA",
            "kid": "ed01",
        }
    ]
}
LIMIT = 20


def log(message):
    print(f"[{time.strftime('%H:%M:%S')}] {message}", flush=True)


def mem_available_kib():
    with open("/proc/meminfo") as handle:
        for line in handle:
            if line.startswith("MemAvailable:"):
                return int(line.split()[1])
    raise RuntimeError("MemAvailable missing")


class MemoryGuard:
    """Kill the watched process group if available memory gets too low."""

    def __init__(self, process):
        self.process = process
        self.low = None
        self.min_seen = mem_available_kib()
        self.stop = threading.Event()
        self.thread = threading.Thread(target=self.run, daemon=True)
        self.thread.start()

    def run(self):
        while not self.stop.wait(0.25):
            available = mem_available_kib()
            self.min_seen = min(self.min_seen, available)
            if available < MIN_AVAILABLE_KIB:
                self.low = available
                try:
                    os.killpg(self.process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                return

    def finish(self):
        self.stop.set()
        self.thread.join()
        if self.low is not None:
            raise SystemExit(f"ABORT: MemAvailable fell to {self.low // 1024} MiB (< 4 GiB)")
        return self.min_seen


def du_bytes(path):
    out = subprocess.run(["du", "-sb", str(path)], check=True, capture_output=True, text=True)
    return int(out.stdout.split()[0])


def timed(argv, label, work):
    """Run argv under /usr/bin/time -v; return wall seconds and peak RSS."""
    report = work / f"time-{label}.txt"
    out = work / f"out-{label}.txt"
    with open(out, "wb") as stdout:
        process = subprocess.Popen(
            ["/usr/bin/time", "-v", "-o", str(report), *argv],
            stdout=stdout,
            stderr=subprocess.STDOUT,
            start_new_session=True,
        )
        guard = MemoryGuard(process)
        code = process.wait()
        min_available = guard.finish()
    text = report.read_text()
    if code != 0:
        raise SystemExit(f"{label} failed with {code}: {out.read_text()[-2000:]}")
    wall = re.search(r"Elapsed \(wall clock\) time \(h:mm:ss or m:ss\): (\S+)", text).group(1)
    parts = [float(p) for p in wall.split(":")]
    seconds = parts[-1] + 60 * parts[-2] + (3600 * parts[-3] if len(parts) == 3 else 0)
    rss = int(re.search(r"Maximum resident set size \(kbytes\): (\d+)", text).group(1))
    user = float(re.search(r"User time \(seconds\): (\S+)", text).group(1))
    system = float(re.search(r"System time \(seconds\): (\S+)", text).group(1))
    return {
        "wall_s": round(seconds, 2),
        "cpu_user_s": user,
        "cpu_sys_s": system,
        "peak_rss_mib": round(rss / 1024, 1),
        "min_mem_available_gib": round(min_available / 1024 / 1024, 1),
    }


def manifest_paths(corpus):
    with open(corpus / "manifest.tsv", encoding="utf-8") as handle:
        next(handle)
        return [line.rstrip("\n").split("\t")[0] for line in handle]


WORD = re.compile(r"[^\W\d_][^\W_]*", re.UNICODE)
STOP = {
    "the", "a", "of", "and", "to", "in", "is", "for", "on", "with", "as", "by", "at", "from",
    "that", "this", "it", "or", "be", "are", "was", "not", "but", "we", "when", "then", "into",
    "over", "after", "before", "related",
}


def derive_queries(vault, paths):
    """~50 deterministic queries drawn from the corpus' own word frequencies."""
    counts = collections.Counter()
    for path in paths[:: max(1, len(paths) // 1500)]:
        text = (vault / path).read_text(encoding="utf-8")
        for word in WORD.findall(text):
            lower = word.lower()
            if lower not in STOP and len(lower) >= 3:
                counts[lower] += 1
    ascii_ranked = [w for w, _ in counts.most_common() if w.isascii()]
    common = ascii_ranked[0:20:2]
    mid = ascii_ranked[400:1400:100]
    rare = [w for w in ascii_ranked if counts[w] == 1][0:400:50]
    queries = []
    queries += [("common", w) for w in common]
    queries += [("mid", w) for w in mid]
    queries += [("rare", w) for w in rare]
    queries += [("two-word", f"{common[i]} {mid[i]}") for i in range(6)]
    queries += [("two-word-rare", f"{mid[i]} {rare[i]}") for i in range(2)]
    queries += [("three-word", f"{common[i]} {mid[i + 3]} {rare[i + 2]}") for i in range(4)]
    queries += [("name", "Galeth Coldwater"), ("name", "Rhoswen"), ("name", "Østergaard")]
    queries += [("non-ascii", "café"), ("non-ascii", "Zürich")]
    queries += [("zero-hit", z) for z in ["zqxjvw", "qqqzzzkkk", "xylophonequark", "vvvbbbnnn", "jjjxxqqq"]]
    return queries


def percentile(samples, p):
    ordered = sorted(samples)
    index = max(0, min(len(ordered) - 1, round(p / 100 * (len(ordered) - 1))))
    return ordered[index]


def summarize(samples_ms):
    return {
        "n": len(samples_ms),
        "p50_ms": round(percentile(samples_ms, 50), 2),
        "p95_ms": round(percentile(samples_ms, 95), 2),
        "max_ms": round(max(samples_ms), 2),
        "mean_ms": round(statistics.fmean(samples_ms), 2),
    }


class Server:
    def __init__(self, kwiry, config, data, prefix, work, label):
        self.log = open(work / f"serve-{label}.err", "wb")
        self.process = subprocess.Popen(
            [str(kwiry), "--config", str(config), "--data-dir", str(data), "serve", "--bind", "127.0.0.1:0"],
            stdout=subprocess.PIPE,
            stderr=self.log,
            start_new_session=True,
        )
        self.started = time.monotonic()
        self.guard = MemoryGuard(self.process)
        line = self.process.stdout.readline().decode()
        if prefix not in line:
            self.stop()
            raise SystemExit(f"serve {label} did not start: {line!r}")
        self.address = line.split("http://", 1)[1].split(";")[0].strip()
        self.host, port = self.address.rsplit(":", 1)
        self.port = int(port)

    def request(self, method, path, headers, body=None):
        connection = http.client.HTTPConnection(self.host, self.port, timeout=120)
        start = time.perf_counter()
        connection.request(method, path, body=body, headers=headers)
        response = connection.getresponse()
        payload = response.read()
        elapsed = (time.perf_counter() - start) * 1000
        result = (response.status, response.getheaders(), payload, elapsed)
        connection.close()
        return result

    def peak_rss_mib(self):
        with open(f"/proc/{self.process.pid}/status") as handle:
            for line in handle:
                if line.startswith("VmHWM:"):
                    return round(int(line.split()[1]) / 1024, 1)
        return None

    def stop(self):
        if self.process.poll() is None:
            os.killpg(self.process.pid, signal.SIGTERM)
            try:
                self.process.wait(timeout=30)
            except subprocess.TimeoutExpired:
                os.killpg(self.process.pid, signal.SIGKILL)
                self.process.wait()
        return self.guard.finish()


def private_key_der():
    source = (REPO / "daemon/crates/kwiry/tests/daemon.rs").read_text()
    block = re.search(r"const PRIVATE_DER: &\[u8\] = &\[(.*?)\];", source, re.S).group(1)
    return bytes(int(b, 16) for b in re.findall(r"0x([0-9a-f]{2})", block))


def b64url(raw):
    return base64.urlsafe_b64encode(raw).rstrip(b"=").decode()


class Minter:
    """Mint live-check-v1 workload capabilities with format_claims()' shape."""

    def __init__(self, work):
        self.key = work / "synthetic-test-key.der"
        self.key.write_bytes(private_key_der())
        self.key.chmod(0o600)
        self.jti = 0
        self.prefix = f"scale-{os.getpid()}-{int(time.time())}"

    def mint(self, ceiling=LIMIT):
        self.jti += 1
        now = int(time.time())
        header = {"alg": "EdDSA", "typ": "kwiry-cap+jwt", "kid": "ed01"}
        claims = {
            "iss": "issuer", "aud": "kwiry-search", "sub": "workload:synthetic-search",
            "jti": f"{self.prefix}-{self.jti}", "iat": now, "nbf": now, "exp": now + 60,
            "tenant": TENANT, "actions": ["search:lexical"],
            "resources": [{"tenant_id": TENANT, "vault_id": VAULT_ID, "room_id": ROOM_ID}],
            "constraints": {"max_limit": ceiling},
        }
        signing_input = f"{b64url(json.dumps(header).encode())}.{b64url(json.dumps(claims).encode())}"
        # Ed25519 is one-shot, so openssl needs the message as a file, not a pipe.
        message = self.key.with_name("signing-input")
        message.write_bytes(signing_input.encode())
        signature = subprocess.run(
            ["openssl", "pkeyutl", "-sign", "-inkey", str(self.key), "-keyform", "DER", "-rawin", "-in", str(message)],
            capture_output=True,
            check=True,
        ).stdout
        return f"{signing_input}.{b64url(signature)}"


def wait_until(predicate, timeout, interval=0.05):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = predicate()
        if value:
            return value
        time.sleep(interval)
    return None


def desktop_body(q):
    return json.dumps({"q": q, "mode": "lexical", "limit": LIMIT}).encode()


def governed_body(q):
    return json.dumps({"limit": LIMIT, "mode": "lexical", "q": q}, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode()


def idle_counters(pid):
    """(CPU seconds, rchar bytes) for a process."""
    with open(f"/proc/{pid}/stat") as handle:
        fields = handle.read().rsplit(")", 1)[1].split()
    cpu = (int(fields[11]) + int(fields[12])) / os.sysconf("SC_CLK_TCK")
    with open(f"/proc/{pid}/io") as handle:
        rchar = next(int(line.split(":")[1]) for line in handle if line.startswith("rchar:"))
    return cpu, rchar


def pick_batch(paths, offset):
    """20 deterministic notes, spread across the tree; offset gives a disjoint set."""
    stride = len(paths) // 21
    chosen = [paths[stride * (i + 1) + offset] for i in range(20)]
    return {"modify": chosen[:10], "delete": chosen[10:15], "rename": chosen[15:20], "tag": f"b{offset}"}


def append(vault, rel, marker):
    with open(vault / rel, "a", encoding="utf-8") as handle:
        handle.write(f"\n\nReconcile check {marker}.\n")


def prepare_batch(vault, batch):
    """Before the first build: give each note that will be deleted or renamed a
    unique word, so its disappearance or new path is observable by search."""
    for i, rel in enumerate(batch["delete"]):
        append(vault, rel, f"scaledel{batch['tag']}x{i}")
    for i, rel in enumerate(batch["rename"]):
        append(vault, rel, f"scaleren{batch['tag']}x{i}")


def apply_batch(vault, batch):
    """Modify 10, delete 5, rename 5 (same folder, new basename)."""
    for i, rel in enumerate(batch["modify"]):
        append(vault, rel, f"scalemod{batch['tag']}x{i}")
    for rel in batch["delete"]:
        (vault / rel).unlink()
    batch["renamed_to"] = []
    for rel in batch["rename"]:
        source = vault / rel
        target = source.with_name(source.stem + " renamed" + source.suffix)
        source.rename(target)
        batch["renamed_to"].append(str(target.relative_to(vault)))


def batch_visible(search, batch):
    """True once every modification, deletion and rename is reflected in search."""
    def hits(q):
        status, _, payload, _ = search(q)
        return json.loads(payload)["hits"] if status == 200 else None

    for i in range(10):
        found = hits(f"scalemod{batch['tag']}x{i}")
        if not found:
            return False
    for i in range(5):
        if hits(f"scaledel{batch['tag']}x{i}") != []:
            return False
    for i, target in enumerate(batch["renamed_to"]):
        found = hits(f"scaleren{batch['tag']}x{i}")
        if not found or {h["path"] for h in found} != {target}:
            return False
    return True


def run_desktop(args, work, vault, queries, results):
    config = work / "desktop" / "config.toml"
    data = work / "desktop" / "data"
    config.parent.mkdir(parents=True)
    base = [str(args.kwiry), "--config", str(config), "--data-dir", str(data)]
    subprocess.run([*base, "vault", "add", "--id", "scale", "--path", str(vault)], check=True, capture_output=True)
    paths = manifest_paths(args.corpus)
    cli_batch, live_batch, boot_batch = (pick_batch(paths, offset) for offset in (0, 1, 2))
    for batch in (cli_batch, live_batch, boot_batch):
        prepare_batch(vault, batch)

    log("desktop: full index")
    build = timed([*base, "index"], "desktop-index", work)
    build["data_dir_bytes"] = du_bytes(data)
    results["desktop_build"] = build
    log(f"desktop build {build}")

    # `kwiry index` always rebuilds a new generation; it is not incremental.
    apply_batch(vault, cli_batch)
    log("desktop: CLI rebuild after 10 modified / 5 deleted / 5 renamed")
    rebuild = timed([*base, "index"], "desktop-cli-rebuild", work)
    rebuild["data_dir_bytes"] = du_bytes(data)
    results["desktop_cli_rebuild_after_batch"] = rebuild
    log(f"desktop CLI rebuild {rebuild}")

    server = Server(args.kwiry, config, data, "kwiry listening on http://", work, "desktop")
    try:
        token = config.with_suffix(".token").read_text().strip()
        headers = {"Authorization": f"Bearer {token}", "Content-Type": "application/json"}

        def search(q):
            return server.request("POST", "/v0/search", headers, desktop_body(q))

        ready = wait_until(lambda: batch_visible(search, cli_batch), 900, 0.2)
        results["desktop_serve_ready_s"] = round(time.monotonic() - server.started, 2)
        if not ready:
            raise SystemExit("desktop serve never reflected the CLI batch")
        status = server.request("GET", "/v0/status", {"Authorization": f"Bearer {token}"})
        results["desktop_status_after_rebuild"] = json.loads(status[2])

        for q in ["warmup", "notes", "index"]:
            search(q)
        passes = []
        per_query = []
        for round_index in range(3):
            samples = []
            for kind, q in queries:
                status_code, _, payload, ms = search(q)
                if status_code != 200:
                    raise SystemExit(f"desktop search {q!r} -> {status_code}: {payload[:300]!r}")
                samples.append(ms)
                if round_index == 0:
                    per_query.append({"kind": kind, "q": q, "hits": len(json.loads(payload)["hits"]), "ms": round(ms, 2)})
            passes.append(samples)
        results["desktop_search"] = {
            "first_pass": summarize(passes[0]),
            "warm_passes": summarize(passes[1] + passes[2]),
            "all": summarize(sum(passes, [])),
        }
        results["queries"] = per_query

        log("desktop: watcher, single edits")
        watch = []
        for i in range(3):
            rel = paths[len(paths) // 7 * (i + 1) + 3]
            marker = f"scalewatchmarker{i}"
            start = time.monotonic()
            append(vault, rel, marker)
            ok = wait_until(lambda: json.loads(search(marker)[2]).get("hits"), 300, 0.05)
            watch.append(round(time.monotonic() - start, 2) if ok else None)
        results["desktop_watch_single_edit_s"] = watch

        log("desktop: watcher, live batch")
        start = time.monotonic()
        apply_batch(vault, live_batch)
        ok = wait_until(lambda: batch_visible(search, live_batch), 900, 0.1)
        results["desktop_live_batch_reconcile_s"] = round(time.monotonic() - start, 2) if ok else None
        results["desktop_serve_peak_rss_mib"] = server.peak_rss_mib()
        log(f"desktop live batch {results['desktop_live_batch_reconcile_s']}")

        # With no changes and no requests, does the daemon settle?
        time.sleep(5)
        before = idle_counters(server.process.pid)
        time.sleep(30)
        after = idle_counters(server.process.pid)
        results["desktop_idle_30s"] = {
            "cpu_s": round(after[0] - before[0], 2),
            "rchar_mib": round((after[1] - before[1]) / 2**20, 1),
        }
        log(f"desktop idle 30 s {results['desktop_idle_30s']}")
    finally:
        results["desktop_serve_min_mem_available_gib"] = round(server.stop() / 1024 / 1024, 1)

    log("desktop: offline batch, boot reconciliation")
    apply_batch(vault, boot_batch)
    server = Server(args.kwiry, config, data, "kwiry listening on http://", work, "desktop-boot")
    try:
        ok = wait_until(lambda: batch_visible(lambda q: server.request("POST", "/v0/search", headers, desktop_body(q)), boot_batch), 900, 0.1)
        results["desktop_boot_batch_reconcile_s"] = round(time.monotonic() - server.started, 2) if ok else None
        results["desktop_boot_serve_peak_rss_mib"] = server.peak_rss_mib()
        log(f"desktop boot batch {results['desktop_boot_batch_reconcile_s']}")
    finally:
        server.stop()
    results["batches"] = {"cli": cli_batch, "live": live_batch, "boot": boot_batch}


def governed_config(work, vault):
    root = work / "governed"
    control = root / "control"
    control.mkdir(parents=True)
    jwks = control / "search.jwks.json"
    jwks.write_text(json.dumps(JWKS))
    config = control / "config.toml"
    config.write_text(
        "version = 1\n\n"
        "[server]\n"
        'profile = "openclast"\n'
        "profile_required = true\n"
        'bind = "127.0.0.1:0"\n\n'
        "[indexing]\n"
        'basis = "strict_hash"\n\n'
        "[auth.openclast]\n"
        f'tenant_id = "{TENANT}"\n'
        'issuer = "issuer"\n'
        'audience = "kwiry-search"\n'
        f"jwks_file = {json.dumps(str(jwks))}\n"
        "max_token_ttl_seconds = 60\n"
        'workload_subject_allowlist = ["workload:synthetic-search"]\n'
        "legacy_identity_accept_until = 0\n\n"
        "[[vaults]]\n"
        f'vault_id = "{VAULT_ID}"\n'
        f'room = "{ROOM_ID}"\n'
        f"path = {json.dumps(str(vault))}\n"
        f'snapshot_id = "{SNAPSHOT_LABEL}"\n'
    )
    return config, root / "index-data"


def describe(headers, body):
    descriptor = []
    for name, value in headers.items():
        if name.lower() == "authorization":
            descriptor.append(["authorization", {
                "ceiling": LIMIT,
                "resources": [{"room_id": ROOM_ID, "tenant_id": TENANT, "vault_id": VAULT_ID}],
                "synthetic_workload": True,
            }])
        else:
            descriptor.append([name.lower(), value])
    return {"method": "POST", "path": "/v0/search", "headers": descriptor, "body": {"utf8": body.decode()}}


def capture(case, headers, body, response):
    status, response_headers, payload, _ = response
    return {
        "case": case,
        "request": describe(headers, body),
        "response": {
            "status": status,
            "headers": {k.lower(): v for k, v in response_headers},
            "body": json.loads(payload),
        },
    }


def run_governed(args, work, vault, queries, results):
    config, data = governed_config(work, vault)
    base = [str(args.kwiry), "--config", str(config), "--data-dir", str(data)]
    log("governed: full index (strict_hash)")
    build = timed([*base, "index"], "governed-index", work)
    build["data_dir_bytes"] = du_bytes(data)
    results["governed_build"] = build
    log(f"governed build {build}")

    minter = Minter(work)
    hit_query = next(q for kind, q in queries if kind == "mid")
    cases = []

    def boot(label):
        """Start the governed daemon; return (server, search) once replay time allows."""
        server = Server(args.kwiry, config, data, "kwiry OpenClast sidecar listening on http://", work, label)
        epoch = int((data / "replay-epoch").read_text())
        wait_until(lambda: int(time.time()) > epoch, 5)

        def search(q):
            h = {
                "Authorization": f"Bearer {minter.mint()}",
                "Kwiry-Profile": "live-check-v1",
                "Content-Type": "application/json",
            }
            body = governed_body(q)
            return h, body, server.request("POST", "/v0/search", h, body)

        return server, search

    def first_200(search, q):
        """Poll with q until the first 200; return its capture inputs."""
        holder = {}

        def attempt():
            h, body, response = search(q)
            holder.update(h=h, body=body, response=response)
            return response[0] == 200

        ok = wait_until(attempt, 900, 0.1)
        return ok, holder

    # Run 1: the first 200 after boot is the "hits" capture; then latency.
    server, search = boot("governed")
    try:
        ok, first = first_200(search, hit_query)
        results["governed_serve_ready_s"] = round(time.monotonic() - server.started, 2)
        if not ok:
            raise SystemExit(f"governed serve never answered 200: {first['response'][0]} {first['response'][2][:300]!r}")
        results["governed_first_200_ms"] = round(first["response"][3], 2)
        cases.append(capture("hits", first["h"], first["body"], first["response"]))
        passes, refused, governed_hits = [], [], []
        for round_index in range(3):
            samples = []
            for kind, q in queries:
                _, _, (status_code, _, payload, ms) = search(q)
                body = json.loads(payload)
                if status_code != 200:
                    refused.append({"t": round(time.monotonic() - server.started, 2), "status": status_code,
                                    "code": body["error"]["code"], "resource_indexes": body["error"].get("resource_indexes")})
                    continue
                samples.append(ms)
                if round_index == 0:
                    governed_hits.append(len(body["hits"]))
            passes.append(samples)
        every = sum(passes, [])
        results["governed_search"] = {
            "answered_200": len(every),
            "refused": len(refused),
            "first_refusal": refused[0] if refused else None,
            "all_200": summarize(every) if every else None,
            "hits_per_query_first_pass": governed_hits,
        }
        log(f"governed search {results['governed_search']['answered_200']} answered, {len(refused)} refused")
        results["governed_serve_peak_rss_mib"] = server.peak_rss_mib()
        before = idle_counters(server.process.pid)
        time.sleep(30)
        after = idle_counters(server.process.pid)
        results["governed_idle_30s_after_queries"] = {
            "cpu_s": round(after[0] - before[0], 2),
            "rchar_mib": round((after[1] - before[1]) / 2**20, 1),
        }
        h, body, response = search(hit_query)
        if response[0] != 200:
            cases.append(capture("gate-closed-after-pass", h, body, response))
    finally:
        results["governed_serve_min_mem_available_gib"] = round(server.stop() / 1024 / 1024, 1)

    # Run 2: the first 200 after a fresh boot is the zero-hit capture.
    server, search = boot("governed-zero")
    try:
        ok, first = first_200(search, "zqxjvw")
        if ok:
            cases.append(capture("no-hits", first["h"], first["body"], first["response"]))
    finally:
        server.stop()
    if args.fixtures:
        fixtures = Path(args.fixtures)
        fixtures.mkdir(parents=True, exist_ok=True)
        (fixtures / "success.json").write_text(json.dumps(cases, indent=2, ensure_ascii=False) + "\n")
        log(f"governed fixtures written to {fixtures}: {[c['case'] for c in cases]}")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--kwiry", type=Path, required=True)
    parser.add_argument("--corpus", type=Path, required=True)
    parser.add_argument("--work", type=Path, required=True)
    parser.add_argument("--fixtures")
    args = parser.parse_args()
    args.kwiry = args.kwiry.resolve()
    args.corpus = args.corpus.resolve()
    work = args.work.resolve()
    if work.exists():
        raise SystemExit(f"work directory already exists: {work}")
    work.mkdir(parents=True)
    results = {"corpus": str(args.corpus), "started": time.strftime("%Y-%m-%dT%H:%M:%S%z")}

    vault = work / "vault"
    log("reflink-copying corpus")
    subprocess.run(["cp", "-a", "--reflink=auto", str(args.corpus / "vault"), str(vault)], check=True)
    queries = derive_queries(vault, manifest_paths(args.corpus))
    results["query_count"] = len(queries)
    try:
        run_desktop(args, work, vault, queries, results)
        # Governed runs on a pristine copy so its room matches the manifest.
        governed_vault = work / "room"
        subprocess.run(["cp", "-a", "--reflink=auto", str(args.corpus / "vault"), str(governed_vault)], check=True)
        shutil.rmtree(vault)
        run_governed(args, work, governed_vault, queries, results)
    finally:
        results["finished"] = time.strftime("%Y-%m-%dT%H:%M:%S%z")
        (work / "results.json").write_text(json.dumps(results, indent=2, ensure_ascii=False) + "\n")
        log(f"results in {work / 'results.json'}")


if __name__ == "__main__":
    sys.exit(main())
