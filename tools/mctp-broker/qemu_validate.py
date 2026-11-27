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
        raise FileNotFoundError(
            f"No built image matching {board}/{profile}/{pattern}; run "
            f"`cargo run -p fbuild -- build --board {board} --debug mctp-validate"
            f"{' --release' if profile == 'release' else ''}` first"
        )
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


def verify_pcapng(pcap_path: Path) -> int:
    """Parse pcapng SHB, IDB (SLL2=276), and Enhanced Packet Blocks carrying ETH_P_MCTP."""
    data = pcap_path.read_bytes()
    off = 0
    saw_shb = False
    saw_idb = False
    mctp_packets = 0
    while off + 12 <= len(data):
        btype, blen = struct.unpack_from("<II", data, off)
        if blen < 12 or (blen & 3) != 0 or off + blen > len(data):
            raise RuntimeError(f"malformed pcapng block at {off} (len={blen})")
        trailer_len = struct.unpack_from("<I", data, off + blen - 4)[0]
        if trailer_len != blen:
            raise RuntimeError(f"pcapng trailing length mismatch at {off}: {trailer_len} != {blen}")
        if btype == 0x0A0D0D0A:
            saw_shb = True
        elif btype == 0x00000001:
            if blen < 20:
                raise RuntimeError("truncated pcapng IDB")
            link_type = struct.unpack_from("<H", data, off + 8)[0]
            if link_type != 276:
                raise RuntimeError(f"expected LINKTYPE_LINUX_SLL2 (276), got {link_type}")
            saw_idb = True
        elif btype == 0x00000006:
            if blen < 32:
                raise RuntimeError("truncated pcapng EPB")
            cap_len = struct.unpack_from("<I", data, off + 20)[0]
            if 28 + cap_len > blen - 4:
                raise RuntimeError(f"EPB cap_len {cap_len} exceeds block length {blen}")
            pkt = data[off + 28 : off + 28 + cap_len]
            if len(pkt) < 24:
                raise RuntimeError(f"short SLL2+MCTP packet ({len(pkt)}B)")
            proto = struct.unpack_from(">H", pkt, 0)[0]
            if proto != 0x00FA:
                raise RuntimeError(f"expected ETH_P_MCTP (0x00FA), got {proto:#x}")
            if (pkt[20] & 0x0F) != 0x01:
                raise RuntimeError(f"bad MCTP version byte {pkt[20]:#x}")
            mctp_packets += 1
        off += blen
    if not (saw_shb and saw_idb and mctp_packets >= 10):
        raise RuntimeError(
            f"pcapng incomplete: shb={saw_shb}, idb={saw_idb}, mctp_packets={mctp_packets}"
        )
    return mctp_packets


def cleanup_procs(*procs: subprocess.Popen | None) -> None:
    for p in procs:
        if p is not None and p.poll() is None:
            p.terminate()
    for p in procs:
        if p is not None:
            try:
                p.wait(timeout=5)
            except subprocess.TimeoutExpired:
                p.kill()
                p.wait(timeout=5)


def test_pcie_doe(q35_bios: Path) -> None:
    print("=== Test 1: qemu-q35 Host SPDM 1.2 over PCIe DOE ===")
    with tempfile.TemporaryDirectory(prefix="fstart-doe-") as tmp:
        nvme_img = Path(tmp) / "nvme.img"
        nvme_img.write_bytes(b"\x00" * (1024 * 1024))
        port = 23231
        broker = None
        qemu = None
        try:
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
            ok = wait_for_marker(qemu, "qemu-q35: SPDM-DOE PASS", 30.0, "q35-doe", log)
            if not ok:
                raise RuntimeError("qemu-q35 PCIe DOE SPDM validation failed")
            print("PASS: qemu-q35 SPDM 1.2 over PCIe DOE (NVMe)")
        finally:
            cleanup_procs(qemu, broker)


def test_dual_qemu_serial(q35_bios: Path, bmc_flash: Path, pcap_path: Path) -> None:
    print("=== Test 2: Dual-QEMU ast2500-evb BMC <-> broker <-> qemu-q35 Host (Serial) ===")
    with tempfile.TemporaryDirectory(prefix="fstart-dual-") as tmp:
        bmc_sock = Path(tmp) / "bmc.sock"
        host_sock = Path(tmp) / "host.sock"
        padded_bmc = Path(tmp) / "bmc-64m.bin"
        raw = bmc_flash.read_bytes()
        target_sz = 64 * 1024 * 1024
        if len(raw) < target_sz:
            padded_bmc.write_bytes(raw + b"\xff" * (target_sz - len(raw)))
        else:
            padded_bmc.write_bytes(raw[:target_sz])

        broker = None
        bmc = None
        host = None
        try:
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
            bmc_log: list[str] = []
            if not wait_for_marker(bmc, "ast2500 mainstage: entered", 90.0, "ast2500-boot", bmc_log):
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
            host_ok = wait_for_marker(host, "qemu-q35: MCTP+PLDM+SPDM-SERIAL PASS", 60.0, "q35-serial", host_log)
            bmc_ok = wait_for_marker(bmc, "ast2500-bmc: MCTP+PLDM+SPDM-SERIAL PASS", 15.0, "ast2500-serial", bmc_log)
            if not (host_ok and bmc_ok):
                raise RuntimeError(f"Dual-QEMU serial failed (host_ok={host_ok}, bmc_ok={bmc_ok})")
            count = verify_pcapng(pcap_path)
            print(f"PASS: Dual-QEMU Serial BMC <-> Host (pcapng: {pcap_path}, {count} verified SLL2 ETH_P_MCTP packets)")
        finally:
            cleanup_procs(host, bmc, broker)


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
