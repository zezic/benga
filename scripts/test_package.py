import contextlib
import io
import os
from pathlib import Path
import stat
import subprocess
import tempfile
import unittest
from unittest.mock import call, patch

import package


class DmgCreationTests(unittest.TestCase):
    def package_with_dmg_results(self, results):
        commands = []
        file_logs = []
        output = io.StringIO()

        def native_command(args, **options):
            if args[:2] == ["hdiutil", "create"]:
                message = results[len(commands)]
                commands.append(args)
                log = options.get("stdout")
                file_logs.append(log is not None and stat.S_ISREG(os.fstat(log.fileno()).st_mode))
                if log is not None:
                    log.write(message or "created disk image\n")
                if message:
                    raise subprocess.CalledProcessError(1, args)
            return subprocess.CompletedProcess(args, 0)

        failure = None
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            stage = root / "stage"
            destination = root / "packages"
            stage.mkdir()
            destination.mkdir()
            binary = root / "benga"
            binary.write_bytes(b"fixture executable")
            with (
                patch.object(package.subprocess, "run", side_effect=native_command),
                patch("time.sleep") as sleep,
                contextlib.redirect_stdout(output),
            ):
                try:
                    package.macos(stage, destination, binary, "Benga-v0.1.0-macos-x86_64", "0.1.0", False)
                except subprocess.CalledProcessError as error:
                    failure = error
        return commands, file_logs, sleep.call_args_list, output.getvalue(), failure

    def test_busy_disk_image_creation_retries_without_rebuilding_the_app(self):
        commands, file_logs, sleeps, output, failure = self.package_with_dmg_results([
            "hdiutil: create failed - Resource busy\n", None,
        ])
        self.assertIsNone(failure)
        self.assertEqual(len(commands), 2)
        self.assertEqual(commands[0], commands[1])
        self.assertEqual(file_logs, [True, True], "hdiutil output must use files rather than pipes")
        self.assertEqual(sleeps, [call(2)])
        self.assertIn("Resource busy", output)

    def test_persistent_busy_error_stops_after_five_attempts(self):
        commands, _, sleeps, output, failure = self.package_with_dmg_results([
            "hdiutil: create failed - Resource busy\n",
        ] * 5)
        self.assertIsInstance(failure, subprocess.CalledProcessError)
        self.assertEqual(len(commands), 5)
        self.assertEqual(sleeps, [call(2), call(4), call(8), call(16)])
        self.assertIn("hdiutil: create failed - Resource busy", output)

    def test_other_creation_errors_fail_immediately(self):
        commands, _, sleeps, _, failure = self.package_with_dmg_results([
            "hdiutil: create failed - No space left on device\n",
        ])
        self.assertIsInstance(failure, subprocess.CalledProcessError)
        self.assertEqual(len(commands), 1)
        self.assertEqual(sleeps, [])


if __name__ == "__main__":
    unittest.main()
