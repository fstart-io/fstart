"""Exercise boot-matrix orchestration without compiling firmware or starting QEMU."""

import json
import os
import subprocess
import sys
import tempfile
import textwrap
import unittest
from pathlib import Path


SCRIPT = Path(__file__).with_name("qemu-boot-tests.sh")


class QemuBootTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        self.assets = self.root / "assets"
        self.assets.mkdir()
        self.x86_assets = self.root / "x86-assets"
        self.x86_assets.mkdir()
        self.calls = self.root / "calls.jsonl"
        self.log_dir = self.root / "logs"
        cargo = self.bin / "cargo"
        cargo.write_text(
            f"#!{sys.executable}\n"
            + textwrap.dedent(
                """\
                import json
                import os
                from pathlib import Path
                import sys
                import time

                args = sys.argv[1:]
                command = args[args.index("--") + 1:]
                with Path(os.environ["MOCK_CALLS"]).open("a") as out:
                    out.write(json.dumps({"args": command,
                                          "smp": os.environ.get("FSTART_QEMU_SMP")}) + "\\n")
                ready = Path(os.environ["MOCK_READY"])
                if command[0] == "build":
                    time.sleep(float(os.environ.get("MOCK_BUILD_DELAY", "0")))
                    status = int(os.environ.get("MOCK_BUILD_STATUS", "0"))
                    if status:
                        print("mock build failed", flush=True)
                        sys.exit(status)
                    ready.touch()
                    if os.environ.get("MOCK_BUILD_MARKER"):
                        print("ramstage: ready for payload", flush=True)
                elif command[0] == "run":
                    # Model fbuild run's implicit cold build for the regression test.
                    if not ready.exists():
                        time.sleep(float(os.environ.get("MOCK_BUILD_DELAY", "0")))
                        ready.touch()
                    if not os.environ.get("MOCK_NO_BOOT_MARKERS"):
                        print("ramstage: ready for payload", flush=True)
                        print("mp: initialization complete (2 CPUs)", flush=True)
                        print("2 CPUs relocated", flush=True)
                        print("SMM: permanent SMI returned on all 2 CPUs", flush=True)
                        print("mp: initialization complete (4 CPUs)", flush=True)
                        print("SMM: selected AMD64 revision 0x00020064; 4 CPUs relocated", flush=True)
                        print("SMM: permanent SMI enabled and SMRAM locked", flush=True)
                        print("SMM: global SMI enable locked and verified", flush=True)
                        print("SMM: permanent SMI returned on all 4 CPUs", flush=True)
                        print("FSTART_CI_BOOT_SUCCESS", flush=True)
                        print("UROOT_BOOT_SUCCESS", flush=True)
                        print("qemu-sbsa ramstage: ready for payload", flush=True)
                        print("PCI root ready (", flush=True)
                    time.sleep(float(os.environ.get("MOCK_BOOT_DELAY", "0")))
                    sys.exit(int(os.environ.get("MOCK_BOOT_STATUS", "0")))
                else:
                    raise AssertionError(command)
                """
            )
        )
        cargo.chmod(0o755)

    def run_matrix(self, board="qemu-q35-i686", payload="halt", **overrides):
        env = dict(
            os.environ,
            PATH=f"{self.bin}{os.pathsep}{os.environ['PATH']}",
            MOCK_CALLS=str(self.calls),
            MOCK_READY=str(self.root / "ready"),
            QEMU_BOOT_LOG_DIR=str(self.log_dir),
            QEMU_BOOT_X86_ASSETS=str(self.x86_assets),
            QEMU_BUILD_TIMEOUT="5s",
            QEMU_BOOT_TIMEOUT="1s",
        )
        env.update(overrides)
        result = subprocess.run(
            ["bash", str(SCRIPT), str(self.assets), "--board", board, "--payload", payload],
            cwd=self.root,
            env=env,
            capture_output=True,
            text=True,
            timeout=15,
        )
        calls = [json.loads(line) for line in self.calls.read_text().splitlines()]
        return result, calls

    def assert_passed(self, result):
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("1 passed, 0 failed", result.stdout)

    def test_cold_build_does_not_consume_boot_timeout(self):
        result, calls = self.run_matrix(MOCK_BUILD_DELAY="2")
        self.assert_passed(result)
        self.assertEqual([call["args"][0] for call in calls], ["build", "run"])

    def test_build_failure_does_not_run_or_reuse_a_stale_boot_log(self):
        self.log_dir.mkdir()
        log = self.log_dir / "qemu-q35-i686-halt.log"
        log.write_text("ramstage: ready for payload\n")
        result, calls = self.run_matrix(MOCK_BUILD_STATUS="17")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("build (exit 17;", result.stdout)
        self.assertEqual([call["args"][0] for call in calls], ["build"])
        self.assertEqual(log.read_text(), "")
        self.assertIn("mock build failed", result.stderr)

    def test_build_has_its_own_deadline(self):
        result, calls = self.run_matrix(QEMU_BUILD_TIMEOUT="0.5s", MOCK_BUILD_DELAY="2")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("build (exit 124;", result.stdout)
        self.assertEqual([call["args"][0] for call in calls], ["build"])

    def test_boot_deadline_still_rejects_missing_markers(self):
        result, calls = self.run_matrix(
            QEMU_BOOT_TIMEOUT="0.5s", MOCK_BOOT_DELAY="2", MOCK_NO_BOOT_MARKERS="1"
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("(exit 124;", result.stdout)
        self.assertNotIn("build (exit", result.stdout)
        self.assertEqual([call["args"][0] for call in calls], ["build", "run"])

    def test_halt_marker_still_passes_when_qemu_is_stopped_by_timeout(self):
        result, _ = self.run_matrix(QEMU_BOOT_TIMEOUT="0.5s", MOCK_BOOT_DELAY="2")
        self.assert_passed(result)

    def test_marker_does_not_hide_a_run_failure(self):
        result, _ = self.run_matrix(MOCK_BOOT_STATUS="9")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("(exit 9;", result.stdout)

    def test_build_output_cannot_supply_the_boot_marker(self):
        result, _ = self.run_matrix(MOCK_BUILD_MARKER="1", MOCK_NO_BOOT_MARKERS="1")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("(exit 0;", result.stdout)

    def test_variant_uses_the_payload_kind_and_preserves_smp(self):
        result, calls = self.run_matrix(board="qemu-q35", payload="halt-smp4-hotplug")
        self.assert_passed(result)
        for call in calls:
            args = call["args"]
            self.assertEqual(args[args.index("--payload") + 1], "halt")
        self.assertEqual(calls[1]["smp"], "4,maxcpus=8")

    def test_image_and_runtime_inputs_remain_on_the_run_command(self):
        cases = [
            ("qemu-riscv64", "linux", ["--kernel", "Image-riscv64", "--firmware", "fw_dynamic.bin"]),
            ("qemu-q35", "uefi-disk", ["--disk", "disk.img"]),
            ("qemu-sbsa", "halt", ["--secure-firmware", "sbsa-secure.pflash"]),
        ]
        (self.assets / "sbsa-secure.pflash").touch()
        (self.x86_assets / "disk.img").touch()
        for board, payload, extra in cases:
            with self.subTest(board=board, payload=payload):
                self.calls.unlink(missing_ok=True)
                result, calls = self.run_matrix(board=board, payload=payload)
                self.assert_passed(result)
                expected = ["--board", board, "--release", "--payload", payload.split("-")[0]]
                self.assertEqual(calls[0]["args"], ["build", *expected])
                extras = [
                    str((self.x86_assets if value == "disk.img" else self.assets) / value)
                    if not value.startswith("--") else value
                    for value in extra
                ]
                self.assertEqual(calls[1]["args"], ["run", *expected, *extras])


if __name__ == "__main__":
    unittest.main()
