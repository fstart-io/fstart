#!/usr/bin/env python3
"""Check a TCG event log against the PCRs a Linux guest printed.

The guest init of ci/tpm/build-guest.sh prints `TPMTEST pcr <bank> <i> <hex>`
(TPM 2.0), `TPMTEST pcrs PCR-xx: ..` (TPM 1.2) and the base64 event log
between TPMTEST_LOG_BEGIN/END. This script parses the log (SHA-1 or
crypto-agile format), replays it per bank and compares PCR 0-9.

    ci/tpm/verify-log.py SERIAL.LOG [--expect TYPE@PCR ...]

Exit status 0 when every replayed PCR matches.
"""
import argparse
import base64
import hashlib
import re
import struct
import sys

ALGS = {0x0004: ("sha1", 20), 0x000B: ("sha256", 32), 0x000C: ("sha384", 48), 0x000D: ("sha512", 64)}
EV_NO_ACTION = 3
EVENT_NAMES = {
    0x01: "EV_POST_CODE", 0x03: "EV_NO_ACTION", 0x04: "EV_SEPARATOR", 0x05: "EV_ACTION",
    0x06: "EV_EVENT_TAG", 0x07: "EV_S_CRTM_CONTENTS", 0x08: "EV_S_CRTM_VERSION",
    0x0A: "EV_PLATFORM_CONFIG_FLAGS", 0x0D: "EV_IPL", 0x0E: "EV_IPL_PARTITION_DATA",
    0x13: "EV_POST_CODE2", 0x80000001: "EV_EFI_VARIABLE_DRIVER_CONFIG",
    0x80000002: "EV_EFI_VARIABLE_BOOT", 0x80000003: "EV_EFI_BOOT_SERVICES_APPLICATION",
    0x80000006: "EV_EFI_GPT_EVENT", 0x80000007: "EV_EFI_ACTION",
    0x80000008: "EV_EFI_PLATFORM_FIRMWARE_BLOB", 0x80000009: "EV_EFI_HANDOFF_TABLES",
    0x8000000A: "EV_EFI_PLATFORM_FIRMWARE_BLOB2", 0x8000000B: "EV_EFI_HANDOFF_TABLES2",
    0x8000000C: "EV_EFI_VARIABLE_BOOT2", 0x800000E0: "EV_EFI_VARIABLE_AUTHORITY",
}


def parse_serial(text):
    pcrs = {}
    for m in re.finditer(r"TPMTEST pcr (\w+) (\d+) ([0-9A-Fa-f]+)", text):
        pcrs.setdefault(m[1], {})[int(m[2])] = bytes.fromhex(m[3])
    for m in re.finditer(r"TPMTEST pcrs PCR-(\d+):((?: [0-9A-F]{2}){20})", text):
        pcrs.setdefault("sha1", {})[int(m[1])] = bytes.fromhex(m[2].replace(" ", ""))
    m = re.search(r"TPMTEST_LOG_BEGIN\r?\n(.*?)TPMTEST_LOG_END", text, re.S)
    if not m:
        sys.exit("no event log in the serial output")
    log = base64.b64decode("".join(m[1].split()))
    return pcrs, log


def parse_log(log):
    """Yield (pcr, type, {alg: digest}, event data)."""
    off = 0
    pcr, etype = struct.unpack_from("<II", log, 0)
    size = struct.unpack_from("<I", log, 28)[0]
    first = log[32:32 + size]
    agile = etype == EV_NO_ACTION and first.startswith(b"Spec ID Event03\0")
    if not agile:
        while off + 32 <= len(log):
            pcr, etype = struct.unpack_from("<II", log, off)
            digest = log[off + 8:off + 28]
            size = struct.unpack_from("<I", log, off + 28)[0]
            yield pcr, etype, {0x0004: digest}, log[off + 32:off + 32 + size]
            off += 32 + size
        return
    n = struct.unpack_from("<I", log, 32 + 24)[0]
    sizes = {}
    for i in range(n):
        alg, dsize = struct.unpack_from("<HH", log, 32 + 28 + 4 * i)
        sizes[alg] = dsize
    yield 0, EV_NO_ACTION, {}, first
    off = 32 + size
    while off + 12 <= len(log):
        pcr, etype, count = struct.unpack_from("<III", log, off)
        off += 12
        digests = {}
        for _ in range(count):
            alg = struct.unpack_from("<H", log, off)[0]
            digests[alg] = log[off + 2:off + 2 + sizes[alg]]
            off += 2 + sizes[alg]
        size = struct.unpack_from("<I", log, off)[0]
        yield pcr, etype, digests, log[off + 4:off + 4 + size]
        off += 4 + size


def describe(etype, data):
    name = EVENT_NAMES.get(etype, hex(etype))
    text = ""
    if etype in (0x13, 0x8000000A, 0x07) and data and data[0] + 17 == len(data):
        text = data[1:1 + data[0]].decode(errors="replace")
    elif etype == 0x08:
        text = data.decode("utf-16-le", errors="replace").rstrip("\0")
    elif etype in (0x05, 0x80000007):
        text = data.decode(errors="replace")
    elif etype == 0x04:
        text = data.hex()
    return name, text


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("serial")
    ap.add_argument("--expect", action="append", default=[],
                    help="EVENT_NAME@PCR that must be in the log")
    args = ap.parse_args()
    with open(args.serial, errors="replace") as serial:
        pcrs, log = parse_serial(serial.read())
    events = list(parse_log(log))
    replay = {}
    seen = set()
    print(f"{len(events)} events, {len(log)} bytes")
    for pcr, etype, digests, data in events:
        name, text = describe(etype, data)
        seen.add(f"{name}@{pcr}")
        print(f"  PCR{pcr:<2} {name:34} {text}")
        if etype == EV_NO_ACTION:
            continue
        for alg, digest in digests.items():
            bank, size = ALGS[alg]
            old = replay.setdefault(bank, {}).get(pcr, bytes(size))
            replay[bank][pcr] = hashlib.new(bank, old + digest).digest()
    ok = True
    if not pcrs:
        print("FAIL: no PCRs in the serial output")
        ok = False
    for bank, values in sorted(pcrs.items()):
        for i in range(10):
            if i not in values:
                continue
            want = replay.get(bank, {}).get(i, bytes(len(values[i])))
            good = values[i] == want
            ok &= good
            print(f"{'ok  ' if good else 'FAIL'} {bank} PCR{i}: {values[i].hex()}"
                  + ("" if good else f" replay {want.hex()}"))
    for expect in args.expect:
        if expect not in seen:
            print(f"FAIL: no {expect} in the log")
            ok = False
    print("PASS" if ok else "FAIL")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
