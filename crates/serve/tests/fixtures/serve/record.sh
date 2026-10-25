#!/usr/bin/env bash
# Record the served-agent contract.
#
#   record.sh python              # start the Python twin and capture into ./python
#   record.sh rust <base-url>     # capture a running Rust server into ./rust
#
# PACKAGES points at the Python sources (default ~/Code/packages); PYTHON at an
# interpreter with fastapi, uvicorn, httpx and websockets.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
packages="${PACKAGES:-$HOME/Code/packages}"
python="${PYTHON:-$packages/newswitch/.venv/bin/python}"
export PYTHONPATH="$packages/rekuest:$packages/fakts:$packages/rath"

case "${1:-python}" in
python)
    db="$(mktemp -d)/twin.db"
    "$python" "$here/twin.py" 8765 "$db" &
    server=$!
    trap 'kill $server 2>/dev/null || true' EXIT
    for _ in $(seq 50); do
        curl -sf http://127.0.0.1:8765/openapi.json >/dev/null && break
        sleep 0.2
    done
    rm -rf "$here/python"
    "$python" "$here/capture.py" http://127.0.0.1:8765 "$here/python"
    ;;
rust)
    rm -rf "$here/rust"
    "$python" "$here/capture.py" "${2:?base url}" "$here/rust"
    ;;
*)
    echo "usage: $0 python | rust <base-url>" >&2
    exit 2
    ;;
esac
