#!/usr/bin/env bash
set -euo pipefail

mode="${1:-local}"
port="${KWIRY_DOCS_PORT:-32190}"
repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
mount_path="/kwiry"

require_command() {
  if ! command -v "$1" >/dev/null 2>&1; then
    printf 'Missing required command: %s\n' "$1" >&2
    exit 1
  fi
}

serve_docs() {
  local interface="$1"
  exec miniserve \
    --interfaces "$interface" \
    --port "$port" \
    --readme \
    "$repo_root"
}

case "$mode" in
  local)
    require_command miniserve
    printf 'Kwiry docs: http://127.0.0.1:%s/\n' "$port"
    serve_docs 127.0.0.1
    ;;

  tailnet)
    require_command miniserve
    require_command tailscale
    require_command python3
    dns_name="$(tailscale status --self --json | python3 -c 'import json,sys; print(json.load(sys.stdin)["Self"]["DNSName"].rstrip("."))')"
    printf 'Kwiry docs (tailnet): http://%s:%s/\n' "$dns_name" "$port"
    serve_docs 0.0.0.0
    ;;

  tailscale)
    require_command miniserve
    require_command tailscale
    require_command python3

    server_pid=""
    cleanup() {
      tailscale serve --set-path="$mount_path" off >/dev/null 2>&1 || true
      if [[ -n "$server_pid" ]]; then
        kill "$server_pid" >/dev/null 2>&1 || true
        wait "$server_pid" 2>/dev/null || true
      fi
    }
    trap cleanup EXIT INT TERM

    miniserve \
      --interfaces 127.0.0.1 \
      --port "$port" \
      --readme \
      "$repo_root" &
    server_pid=$!

    for _ in {1..50}; do
      if (exec 3<>"/dev/tcp/127.0.0.1/$port") 2>/dev/null; then
        exec 3>&-
        break
      fi
      if ! kill -0 "$server_pid" 2>/dev/null; then
        wait "$server_pid"
        exit 1
      fi
      sleep 0.1
    done

    tailscale serve --bg --set-path="$mount_path" "http://127.0.0.1:$port"
    dns_name="$(tailscale status --self --json | python3 -c 'import json,sys; print(json.load(sys.stdin)["Self"]["DNSName"].rstrip("."))')"
    printf 'Kwiry docs (tailnet HTTPS): https://%s%s/\n' "$dns_name" "$mount_path"
    printf 'Logo review: https://%s%s/docs/logo/kwiry-preview.html\n' "$dns_name" "$mount_path"
    printf 'Press Ctrl+C to stop the server and remove only the %s mount.\n' "$mount_path"

    wait "$server_pid"
    ;;

  unmount)
    require_command tailscale
    tailscale serve --set-path="$mount_path" off
    printf 'Removed the Tailscale Serve mount at %s.\n' "$mount_path"
    ;;

  *)
    printf 'Usage: %s {local|tailnet|tailscale|unmount}\n' "$0" >&2
    exit 2
    ;;
esac
