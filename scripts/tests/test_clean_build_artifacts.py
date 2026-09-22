"""Exercise destructive-cleanup boundaries using disposable workspaces only."""
import fcntl
from pathlib import Path
import subprocess
import tempfile
import unittest

SOURCE = Path(__file__).resolve().parents[1] / "clean-build-artifacts.py"


class CleanupTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="build-cleanup-test-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.script = self.root / "scripts/clean-build-artifacts.py"
        self.script.parent.mkdir()
        self.script.write_text(SOURCE.read_text())
        self.target = self.root / "target"
        self.cache = self.target / "debug/deps/cache"
        self.cache.parent.mkdir(parents=True)
        self.cache.write_text("rebuildable")
        (self.target / "CACHEDIR.TAG").write_text("Signature: 8a477f597d28d172789f06886806bc55\n")
        self.binary = self.target / "debug/haarness"
        self.binary.write_text("keep executable")
        self.data = self.root / "sessions.db"
        self.data.write_text("keep data")

    def run_cleaner(self, *args):
        return subprocess.run(["python3", str(self.script), *args], capture_output=True, text=True)

    def test_preview_and_apply_preserve_outputs_and_user_data(self):
        release = self.target / "release/haarness"
        release.parent.mkdir()
        release.write_text("keep release")
        result = self.run_cleaner()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(self.cache.exists())
        result = self.run_cleaner("--apply")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse(self.cache.exists())
        self.assertEqual(self.binary.read_text(), "keep executable")
        self.assertEqual(self.data.read_text(), "keep data")
        self.assertEqual(release.read_text(), "keep release")

    def test_busy_cargo_lock_blocks_removal(self):
        with (self.target / "debug/.cargo-lock").open("w") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            result = self.run_cleaner("--apply")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Cargo is using", result.stderr)
        self.assertTrue(self.cache.exists())

    def test_open_artifact_blocks_removal(self):
        with self.cache.open():
            result = self.run_cleaner("--apply")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Artifact is in use", result.stderr)
        self.assertTrue(self.cache.exists())

    def test_symlinked_cache_and_missing_marker_are_rejected(self):
        (self.target / "debug/incremental").symlink_to(self.script.parent, target_is_directory=True)
        self.assertNotEqual(self.run_cleaner("--apply").returncode, 0)
        self.assertTrue(self.script.exists())
        (self.target / "debug/incremental").unlink()
        (self.target / "CACHEDIR.TAG").unlink()
        self.assertNotEqual(self.run_cleaner("--apply").returncode, 0)
        self.assertTrue(self.cache.exists())

    def test_incremental_only_preserves_dependency_cache(self):
        incremental = self.target / "debug/incremental"
        incremental.mkdir()
        (incremental / "state").write_text("compiler state")
        result = self.run_cleaner("--incremental-only", "--apply")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse(incremental.exists())
        self.assertTrue(self.cache.exists())


if __name__ == "__main__":
    unittest.main()
