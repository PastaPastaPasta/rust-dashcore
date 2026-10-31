"""A gate holder that never unshares (D67's negative): it signals ready from the harness's own namespaces."""

import json
import os
import sys
import time

with open(sys.argv[sys.argv.index("--config") + 1]) as f:
    os.write(json.load(f)["ready_fd"], b"R")
time.sleep(300)
