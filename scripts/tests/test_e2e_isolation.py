"""The end-to-end harnesses keep a test run away from the person's home and from Haarness Cloud: no usage
statistics are reported, and the harnesses that pass only `--data-dir` give the run a home of its own."""
import ast
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts"))
sys.path.insert(0, str(ROOT / "sdks" / "tests"))

import e2e_tmux  # noqa: E402


class UsageStatisticsTests(unittest.TestCase):
    def test_a_tmux_pane_keeps_usage_statistics_off_unless_the_script_says_otherwise(self):
        command = e2e_tmux.pane_command({"HAARNESS_HOME": "/tmp/h"}, ["haarness"])
        self.assertIn("HAARNESS_TELEMETRY=0", command)
        command = e2e_tmux.pane_command({"HAARNESS_TELEMETRY": "1"}, ["haarness"])
        self.assertIn("HAARNESS_TELEMETRY=1", command)
        self.assertNotIn("HAARNESS_TELEMETRY=0", command)

    def test_the_sdk_fixture_server_reports_no_usage_statistics(self):
        from server import Fixture

        fixture = Fixture()
        try:
            self.assertEqual(fixture.env["HAARNESS_TELEMETRY"], "0")
        finally:
            fixture.tmp.cleanup()

    def test_a_clean_environment_drops_the_persons_settings_and_puts_isolation_back(self):
        import e2e_env

        env = e2e_env.clean_env(
            "AWS_",
            base={"PATH": "/bin", "HAARNESS_HOME": "/real", "HAARNESS_TELEMETRY": "1", "AWS_PROFILE": "p"},
        )
        self.assertEqual(env, {"PATH": "/bin", **e2e_env.ISOLATED})
        self.assertEqual(e2e_env.ISOLATED["HAARNESS_TELEMETRY"], "0")

    def test_no_harness_leaves_out_every_haarness_setting_without_putting_isolation_back(self):
        # Dropping every HAARNESS_* variable also drops HAARNESS_TELEMETRY=0 and a closed
        # HAARNESS_CLOUD_API_URL, so a script uses e2e_env.clean_env (a spec, isolated-env.ts) instead.
        drop = re.compile(
            r"not\s+\w+\.startswith\(\s*\(?\s*['\"]HAARNESS_"
            r"|startswith\(\s*['\"]HAARNESS_['\"]\s*\)\s+and\s+\w+\s+not\s+in"
        )
        allowlist = re.compile(r"os\.environ\.items\(\)\s+if\s+\w+\s+in\s+\(")
        python = sorted([*(ROOT / "scripts").glob("*.py"), *(ROOT / "sdks/tests").glob("*.py")])
        self.assertGreater(len(python), 50)
        for path in python:
            text = path.read_text()
            self.assertIsNone(drop.search(text), path.name)
            if "clean_env(" in text and path.name != "e2e_env.py":
                imports = [node for node in ast.walk(ast.parse(text)) if isinstance(node, ast.ImportFrom)]
                self.assertTrue(any(node.module == "e2e_env" for node in imports), path.name)
            if allowlist.search(text):
                self.assertTrue("ISOLATED" in text or "HAARNESS_TELEMETRY" in text, path.name)
        specs = sorted((ROOT / "apps/desktop/e2e").rglob("*.ts"))
        self.assertGreater(len(specs), 30)
        for path in specs:
            if path.name != "isolated-env.ts":
                self.assertNotIn('startsWith("HAARNESS_")', path.read_text(), path.name)
        for path, line in [
            ("apps/desktop/playwright.config.ts", 'process.env.HAARNESS_TELEMETRY ??= "0";'),
            ("scripts/test-all.sh", 'export HAARNESS_TELEMETRY="${HAARNESS_TELEMETRY:-0}"'),
        ]:
            self.assertTrue(line in (ROOT / path).read_text(), f"{path} lacks {line}")


class OwnHomeTests(unittest.TestCase):
    def test_the_session_archive_script_runs_haarness_with_a_home_of_its_own(self):
        with tempfile.TemporaryDirectory(prefix="haarness-isolation-test-") as temp:
            temp = Path(temp).resolve()
            person = temp / "person"
            person.mkdir()
            seen = temp / "seen.txt"
            # Stands in for haarness: says which home and data folder it was given, then fails, which ends the
            # script after its first command.
            binary = temp / "haarness"
            binary.write_text(
                "#!/bin/sh\n"
                f'printf "%s|%s|%s|%s\\n" "$HOME" "${{HAARNESS_DATA_DIR:-}}" "${{HAARNESS_TELEMETRY:-}}" '
                f'"${{HAARNESS_CLOUD_API_URL:-}}" >> {seen}\n'
                "exit 1\n"
            )
            binary.chmod(0o755)
            env = {
                **os.environ,
                "HOME": str(person),
                "HAARNESS_DATA_DIR": str(person / "data"),
                "HAARNESS_BIN": str(binary),
                "TMPDIR": str(temp),
            }
            env.pop("HAARNESS_TELEMETRY", None)
            subprocess.run(["sh", str(ROOT / "scripts/test-session-archive.sh")], env=env, capture_output=True)
            lines = seen.read_text().splitlines()
            self.assertTrue(lines)
            for line in lines:
                home, data, telemetry, cloud = line.split("|")
                self.assertTrue(home.startswith(str(temp)) and not home.startswith(str(person)), line)
                self.assertEqual((data, telemetry, cloud), ("", "0", "http://127.0.0.1:9"), line)


if __name__ == "__main__":
    unittest.main()
