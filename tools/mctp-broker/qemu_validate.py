#!/usr/bin/env python3
"""End-to-end QEMU validation for fstart MCTP + PLDM + SPDM (Host & BMC).

Runs:
  1. Host qemu-q35 SPDM 1.2 over PCIe DOE (NVMe spdm_port <-> fstart-mctp-broker spdm-emu)
  2. Dual-QEMU ast2500-evb BMC <-> fstart-mctp-broker (Wireshark SLL2 ETH_P_MCTP pcapng)
     <-> qemu-q35 Host over Serial (DSP0253)
"""

import glob
import os
import struct
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
BROKER = ROOT / "target" / "debug" / "fstart-mctp-broker"


def find_image(board: str, profile: str, pattern: str) -> Path:
    candidates = sorted(
        glob.glob(str(ROOT / "target" / "fstart-build" / board / profile / "*" / "image" / "*" / pattern)),
        key=os.path.getmtime,
        reverse=True,
    )
    if not candidates:
        raise FileNotFoundError(f"No built image matching {board}/{profile}/{pattern}; run fbuild first")
    return Path(candidates[0])


def wait_for_marker(proc: subprocess.Popen, marker: str, timeout: float, label: str, log: list[str]) -> bool:
    deadline = time.monotonic() + timeout
    found = threading.Event()

    def reader():
        assert proc.stdout is not None
        for raw in iter(proc.stdout.readline, b""):
            line = raw.decode("utf-8", errors="replace").rstrip()
            log.append(line)
            if marker in line:
                found.set()
                break

    t = threading.Thread(target=reader, daemon=True)
    t.start()
    ok = found.wait(max(0.1, deadline - time.monotonic()))
    if not ok:
        print(f"[{label}] TIMEOUT waiting for {marker!r}. Last 20 lines:", file=sys.stderr)
        for l in log[-20:]:
            print(f"  {l}", file=sys.stderr)
    return ok


def test_pcie_doe(q35_bios: Path) -> None:
    print("=== Test 1: qemu-q35 Host SPDM 1.2 over PCIe DOE ===")
    with tempfile.TemporaryDirectory(prefix="fstart-doe-") as tmp:
        nvme_img = Path(tmp) / "nvme.img"
        nvme_img.write_bytes(b"\x00" * (1024 * 1024))
        port = 23231
        broker = subprocess.Popen(
            [str(BROKER), "spdm-emu", "--port", str(port)],
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
        )
        time.sleep(0.3)
        qemu = subprocess.Popen(
            [
                "qemu-system-x86_64",
                "-machine", "q35",
                "-accel", "tcg",
                "-cpu", "max",
                "-m", "1G",
                "-no-reboot",
                "-vga", "none",
                "-nographic",
                "-bios", str(q35_bios),
                "-drive", f"file={nvme_img},format=raw,if=none,id=drv0",
                "-device", f"nvme,serial=fstart0,drive=drv0,spdm_port={port},spdm_trans=doe",
            ],
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
        )
        log: list[str] = []
        try:
            ok = wait_for_marker(qemu, "qemu-q35: SPDM-DOE PASS", 30.0, "q35-doe", log)
            if not ok:
                raise RuntimeError("qemu-q35 PCIe DOE SPDM validation failed")
            print("PASS: qemu-q35 SPDM 1.2 over PCIe DOE (NVMe)")
        finally:
            qemu.terminate()
            broker.terminate()
            qemu.wait(timeout=5)
            broker.wait(timeout=5)


def test_dual_qemu_serial(q35_bios: Path, bmc_flash: Path, pcap_path: Path) -> None:
    print("=== Test 2: Dual-QEMU ast2500-evb BMC <-> broker <-> qemu-q35 Host (Serial) ===")
    with tempfile.TemporaryDirectory(prefix="fstart-dual-") as tmp:
        bmc_sock = Path(tmp) / "bmc.sock"
        host_sock = Path(tmp) / "host.sock"
        # Pad BMC flash image to 64 MiB for ast2500-evb MTD if smaller
        padded_bmc = Path(tmp) / "bmc-64m.bin"
        raw = bmc_flash.read_bytes()
        target_sz = 64 * 1024 * 1024
        if len(raw) < target_sz:
            padded_bmc.write_bytes(raw + b"\xff" * (target_sz - len(raw)))
        else:
            padded_bmc.write_bytes(raw[:target_sz])

        broker = subprocess.Popen(
            [
                str(BROKER),
                "bridge",
                "--bmc-sock", str(bmc_sock),
                "--host-sock", str(host_sock),
                "--transport", "serial",
                "--pcap", str(pcap_path),
            ],
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
        )
        for _ in range(50):
            if bmc_sock.exists() and host_sock.exists():
                break
            time.sleep(0.05)

        bmc = subprocess.Popen(
            [
                "qemu-system-arm",
                "-M", "ast2500-evb,execute-in-place=on,fmc-model=mt25ql512ab",
                "-no-reboot",
                "-display", "none",
                "-monitor", "none",
                "-drive", f"file={padded_bmc},format=raw,if=mtd",
                "-chardev", f"socket,id=mctp,path={bmc_sock}",
                "-serial", "stdio",
                "-serial", "chardev:mctp",
            ],
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
        )
        # Give AST2500 bootblock time to verify Ed25519 + load mainstage on ARM1176 TCG
        bmc_log: list[str] = []
        if not wait_for_marker(bmc, "ast2500 mainstage: entered", 90.0, "ast2500-boot", bmc_log):
            bmc.terminate()
            broker.terminate()
            raise RuntimeError("ast2500-evb did not reach mainstage")

        host = subprocess.Popen(
            [
                "qemu-system-x86_64",
                "-machine", "q35",
                "-accel", "tcg",
                "-cpu", "max",
                "-m", "1G",
                "-no-reboot",
                "-vga", "none",
                "-display", "none",
                "-monitor", "none",
                "-bios", str(q35_bios),
                "-chardev", f"socket,id=mctp,path={host_sock}",
                "-serial", "stdio",
                "-serial", "chardev:mctp",
            ],
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
        )
        host_log: list[str] = []
        try:
            host_ok = wait_for_marker(host, "qemu-q35: MCTP+PLDM+SPDM-SERIAL PASS", 60.0, "q35-serial", host_log)
            bmc_ok = wait_for_marker(bmc, "ast2500-bmc: MCTP+PLDM+SPDM-SERIAL PASS", 15.0, "ast2500-serial", bmc_log)
            if not (host_ok and bmc_ok):
                raise RuntimeError(f"Dual-QEMU serial failed (host_ok={host_ok}, bmc_ok={bmc_ok})")
            # Verify pcapng was written with LINKTYPE_LINUX_SLL2 (276) and EPB packets
            data = pcap_path.read_bytes()
            assert len(data) > 100, f"pcapng too small: {len(data)} bytes"
            link_type = struct.unpack_from("<H", data, 28 + 8)[0]
            assert link_type == 276, f"unexpected link_type={link_type}"
            print(f"PASS: Dual-QEMU Serial BMC <-> Host (pcapng: {pcap_path}, {len(data)} bytes, SLL2=276)")
        finally:
            host.terminate()
            bmc.terminate()
            broker.terminate()
            host.wait(timeout=5)
            bmc.wait(timeout=5)
            broker.wait(timeout=5)


def main() -> int:
    q35_bios = find_image("qemu-q35", "debug", "qemu-q35.rom")
    bmc_flash = find_image("gigabyte-mj11-ec1-bmc", "release", "*-64m.pflash")
    print(f"Using q35 BIOS:  {q35_bios}")
    print(f"Using BMC flash: {bmc_flash}")
    test_pcie_doe(q35_bios)
    test_dual_qemu_serial(q35_bios, bmc_flash, Path("/tmp/dual-qemu-serial.pcapng"))
    return 0


if __name__ == "__main__":
    sys.exit(main())
