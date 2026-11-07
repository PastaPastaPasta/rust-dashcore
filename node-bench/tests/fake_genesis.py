#!/usr/bin/python3
"""A stand-in for rdashd under the startup-genesis recipe: run -startupnotify through the shell, then exit
$FAKE_GENESIS_EXIT (default 0) on SIGTERM."""

import os
import signal
import sys
import threading

notify = next(a.split("=", 1)[1] for a in sys.argv if a.startswith("-startupnotify="))
stop = threading.Event()
signal.signal(signal.SIGTERM, lambda *_: stop.set())
os.system(notify)
stop.wait(60)
sys.exit(int(os.environ.get("FAKE_GENESIS_EXIT", "0")) if stop.is_set() else 3)
