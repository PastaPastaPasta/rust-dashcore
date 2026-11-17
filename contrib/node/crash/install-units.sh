#!/usr/bin/env bash
# Installs ST3b's systemd user units (PLAN-ST3b §10, Q13): copies contrib/node/crash/units/* (the
# units and their drop-in directories) to ~/.config/systemd/user/ and reloads. With --enable it
# also enables and starts the timers; without it, nothing is scheduled, and a run can be started
# by hand:
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
# Drop-ins: the VM weekly's per-component timeouts (st3b-vm-weekly@<component>.service.d/).
for d in "$src"/*.service.d; do
  [ -d "$d" ] || continue
  dropin=$dst/$(basename "$d")
  mkdir -p "$dropin"
  for f in "$d"/*.conf; do
    install -m 0644 "$f" "$dropin/"
    echo "installed $dropin/$(basename "$f")"
  done
done
systemctl --user daemon-reload
if [ "${1:-}" = --enable ]; then
  for t in "$src"/*.timer; do
    systemctl --user enable --now "$(basename "$t")"
  done
fi
