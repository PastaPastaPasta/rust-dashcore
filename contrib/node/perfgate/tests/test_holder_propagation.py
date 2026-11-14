"""The holder's mount namespace follows the host's unmounts (BL-510): a mount live when the holder starts and
removed by the host while it runs is gone from the holder's view, so the holder doesn't pin it.

The "host" is root of its own user and mount namespace with a shared tmpfs base (as systemd makes / shared), so
the test needs no privilege; the holder is started with the production argv, invocation.holder_unshare().

    python3 -m unittest discover -s contrib/node/perfgate/tests -p test_holder_propagation.py
"""

import os
import shlex
import shutil
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.realpath(__file__))))

from perfgate import invocation as iv  # noqa: E402

UNSHARE = shutil.which("unshare")

# Runs as the "host". Prints "before N after M": the holder's mountinfo lines for the test mount around the
# host's umount.
HOST = """
set -e
base=$1; shift
mount -t tmpfs -o size=1m pgbase "$base"
mount --make-shared "$base"
mkdir "$base/mp"
mount -t tmpfs -o size=1m pgtest "$base/mp"
"$@" sleep 60 >/dev/null 2>&1 &
outer=$!
trap 'kill -KILL $outer 2>/dev/null' EXIT
# The child counts once it runs sleep: before its exec, unshare may not have set the propagation yet.
for i in $(seq 200); do
  kid=$(cat /proc/$outer/task/$outer/children 2>/dev/null | tr -d ' ')
  [ -n "$kid" ] && [ "$(cat /proc/$kid/comm 2>/dev/null)" = sleep ] && break
  kid=; sleep 0.05
done
[ -n "$kid" ] || { echo "no holder child"; exit 3; }
before=$(grep -c " $base/mp " /proc/$kid/mountinfo || true)
umount "$base/mp"
after=$(grep -c " $base/mp " /proc/$kid/mountinfo || true)
echo "before $before after $after"
"""


def userns_unavailable():
    if not UNSHARE:
        return "no unshare"
    if subprocess.run([UNSHARE, "--user", "--map-root-user", "--mount", "true"], capture_output=True).returncode:
        return "no unprivileged user namespaces"
    return None


SKIP = userns_unavailable()


@unittest.skipIf(SKIP, SKIP or "")
class HolderPropagation(unittest.TestCase):
    def run_host(self, holder):
        base = tempfile.mkdtemp(prefix="prop-", dir=os.environ.get("PERFGATE_TEST_DIR"))
        self.addCleanup(shutil.rmtree, base, True)
        p = subprocess.run([UNSHARE, "--user", "--map-root-user", "--mount", "--propagation", "unchanged", "--",
                            "bash", "-c", HOST, "host", base, *holder], capture_output=True, text=True, timeout=60)
        self.assertEqual(p.returncode, 0, p.stdout + p.stderr)
        return p.stdout.strip()

    def test_host_unmount_reaches_the_holder(self):
        self.assertEqual(self.run_host(iv.holder_unshare({"unshare": UNSHARE})), "before 1 after 0")

    def test_the_old_private_holder_pins_it(self):
        """The control: without slave propagation the holder keeps the mount, so the test above can fail."""
        private = [UNSHARE, "--user", "--map-root-user", "--mount", "--fork", "--kill-child", "--"]
        self.assertEqual(self.run_host(private), "before 1 after 1", shlex.join(private))


if __name__ == "__main__":
    unittest.main()
