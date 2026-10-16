#!/usr/bin/env python3
"""Failed image activation must reclaim its owned probe VM and volume.

Requires the same independent MSB_HOME used by the qualification Host.
Never run this against a production runtime home.
"""
import os
import secrets
import subprocess
import tempfile
import time
import unittest
from pathlib import Path


class FailedProbeQualification(unittest.TestCase):
    def test_failed_activation_does_not_leave_owned_resources(self):
        root = Path(__file__).resolve().parents[1]
        directory = Path(os.environ["AGENT_SPACE_BROWSER_TEST_DIR"])
        env = os.environ.copy()
        env["MSB_HOME"] = os.environ["AGENT_SPACE_BROWSER_TEST_MSB_HOME"]
        env["QUAL_PROBE_ADMIN_TOKEN"] = secrets.token_hex(32)
        binary = root / "target/debug/agent-space-mcp"
        prefix = f"qualfail{os.getpid()}{int(time.time())}"
        with tempfile.TemporaryDirectory(dir=directory) as tmp:
            config = Path(tmp) / "config.toml"
            config.write_text(
                f'resource_prefix="{prefix}"\n'
                '[server]\nlisten="127.0.0.1:0"\nmax_sessions=1\n'
                '[engine]\ntype="msb"\n'
                f'[state]\ndir="{tmp}/state"\n'
                '[admin]\ntoken_env="QUAL_PROBE_ADMIN_TOKEN"\n'
                '[workspace]\nimage="127.0.0.1:1/qualification/missing:fixed"\n'
                'cpus=1\nmemory_mib=256\nworkspace_size_mib=64\n'
                'shell="/bin/bash"\n'
                '[lifecycle]\nidle_timeout_secs=120\nsession_timeout_secs=60\n'
                'check_interval_secs=30\n[logging]\nlevel="info"\n'
            )
            command = [str(binary), "--config", str(config)]
            initialized = subprocess.run(
                command + ["init-state"], env=env, capture_output=True, timeout=20
            )
            self.assertEqual(initialized.returncode, 0, initialized.stderr.decode())
            failed = subprocess.run(command, env=env, capture_output=True, timeout=45)
            self.assertNotEqual(failed.returncode, 0)
            msb = str(Path(env["MSB_HOME"]) / "bin/msb")
            for resource in ("sandbox", "volume"):
                names = subprocess.check_output(
                    [msb, resource, "list", "-q"], env=env, text=True
                )
                self.assertFalse(any(n.startswith(prefix + "-") for n in names.splitlines()))


if __name__ == "__main__":
    unittest.main(verbosity=2)
