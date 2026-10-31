#!/usr/bin/env bash
# Installs ST3b's systemd user units (PLAN-ST3b §10, Q13): copies contrib/node/crash/units/* to
# ~/.config/systemd/user/ and reloads. With --enable it also enables and starts the timers; without
# it, nothing is scheduled, and a run can be started by hand:
#   systemctl --user start st3b-device-nightly.service
#
#   contrib/node/crash/install-units.sh [--enable]
set -euo pipefail

src=$(cd "$(dirname "$0")/units" && pwd)
dst=${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user
mkdir -p "$dst"
for f in "$src"/*.service "$src"/*.timer; do
  install -m 0644 "$f" "$dst/"
  echo "installed $dst/$(basename "$f")"
done
systemctl --user daemon-reload
if [ "${1:-}" = --enable ]; then
  for t in "$src"/*.timer; do
    systemctl --user enable --now "$(basename "$t")"
  done
fi
