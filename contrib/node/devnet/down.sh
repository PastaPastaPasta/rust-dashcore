#!/usr/bin/env bash
set -euo pipefail
here=$(cd -- "$(dirname -- "$0")" && pwd)
exec python3 -I -B "$here/lifecycle.py" down "$@"
