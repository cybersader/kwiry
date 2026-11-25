"""Test cancellation with GNU time and a harmless sleeping child (no model)."""

import os
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent

DRIVER = """
import os, sys
from pathlib import Path
sys.path.insert(0, sys.argv[1])
import drive_meaning as driver
# The sleep fixture does not need 4 GiB of available RAM.
driver.drive.mem_available_kib = lambda: driver.drive.MIN_AVAILABLE_KIB
try:
    with driver.managed_helper(sys.argv[2:], None) as timed:
        Path('wrapper').write_text(str(timed.pid))
        import time
        time.sleep(60)
except SystemExit:
    assert timed.returncode is not None, 'timing process not reaped'
    raise
"""

SLEEPER = """
import os, signal, time
from pathlib import Path
def exit_on_signal(sig, _frame):
    Path('received').write_text(str(sig))
    raise SystemExit(0)
for sig in (signal.SIGTERM, signal.SIGINT):
    signal.signal(sig, exit_on_signal)
Path('child').write_text(str(os.getpid()))
Path('ready').touch()
time.sleep(60)
"""


class BenchmarkCancellation(unittest.TestCase):
    def test_forwards_sigterm_to_timed_child_and_reaps(self):
        self.check_cancel(signal.SIGTERM)

    def test_forwards_sigint_to_timed_child_and_reaps(self):
        self.check_cancel(signal.SIGINT)

    def check_cancel(self, sig):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            command = [sys.executable, "-IB", "-c", DRIVER, str(HERE), "/usr/bin/time",
                       "-v", "-o", "time.log", sys.executable, "-IB", "-c", SLEEPER]
            driver = subprocess.Popen(command, cwd=root)
            try:
                deadline = time.monotonic() + 10
                while not all((root / f).exists() for f in ("ready", "wrapper")):
                    self.assertIsNone(driver.poll(), "driver exited before fixture was ready")
                    self.assertLess(time.monotonic(), deadline, "fixture did not start")
                    time.sleep(0.01)
                wrapper = int((root / "wrapper").read_text())
                child = int((root / "child").read_text())
                driver.send_signal(sig)
                self.assertEqual(driver.wait(timeout=10), 128 + sig)
                self.assertEqual(int((root / "received").read_text()), sig)
                self.assertFalse(Path(f"/proc/{wrapper}").exists(), "timing process still exists")
                self.assertFalse(Path(f"/proc/{child}").exists(), "sleeping child still exists")
            finally:
                if driver.poll() is None:
                    driver.terminate()
                    try:
                        driver.wait(timeout=10)
                    except subprocess.TimeoutExpired:
                        driver.kill()
                        driver.wait()
                if (root / "wrapper").exists():
                    try:
                        os.killpg(int((root / "wrapper").read_text()), signal.SIGKILL)
                    except ProcessLookupError:
                        pass


if __name__ == "__main__":
    unittest.main()
