#!/usr/bin/env python3
"""Check a TCG event log against the PCRs a Linux guest printed.

The guest init of ci/tpm/build-guest.sh prints `TPMTEST pcr <bank> <i> <hex>`
(TPM 2.0), `TPMTEST pcrs PCR-xx: ..` (TPM 1.2) and the base64 event log
between TPMTEST_LOG_BEGIN/END. This script parses the log (SHA-1 or
crypto-agile format), replays it per bank and compares PCR 0-9.

    ci/tpm/verify-log.py SERIAL.LOG [--expect TYPE@PCR ...]

Exit status 0 only with PCR 0-9 for every expected/logged bank, matching
replay, all requested events, and successful separators for PCR 0-7.
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
        raise ValueError("no event log in the serial output")
    log = base64.b64decode("".join(m[1].split()), validate=True)
    return pcrs, log


def parse_log(log):
    """Yield (pcr, type, {alg: digest}, event data)."""
    off = 0

    def take(size):
        nonlocal off
        if size > len(log) - off:
            raise ValueError(f"truncated event log at offset {off}")
        data = log[off:off + size]
        off += size
        return data

    def unpack(fmt):
        return struct.unpack(fmt, take(struct.calcsize(fmt)))

    pcr, etype = unpack("<II")
    digest = take(20)
    first = take(unpack("<I")[0])
    if pcr != 0 or etype != EV_NO_ACTION or first[:16] not in (
        b"Spec ID Event00\0", b"Spec ID Event02\0", b"Spec ID Event03\0"
    ):
        raise ValueError("missing Spec ID event")
    agile = first.startswith(b"Spec ID Event03\0")
    if not agile:
        yield pcr, etype, {0x0004: digest}, first
        while off < len(log):
            pcr, etype = unpack("<II")
            digest = take(20)
            data = take(unpack("<I")[0])
            yield pcr, etype, {0x0004: digest}, data
        return
    if len(first) < 29:
        raise ValueError("truncated Spec ID event")
    n = struct.unpack_from("<I", first, 24)[0]
    if not 1 <= n <= len(ALGS) or len(first) < 29 + 4 * n:
        raise ValueError("invalid Spec ID algorithm list")
    sizes = {}
    for i in range(n):
        alg, dsize = struct.unpack_from("<HH", first, 28 + 4 * i)
        if alg not in ALGS or ALGS[alg][1] != dsize or alg in sizes:
            raise ValueError("invalid Spec ID algorithm or digest size")
        sizes[alg] = dsize
    if len(first) != 29 + 4 * n + first[28 + 4 * n]:
        raise ValueError("invalid Spec ID vendor info length")
    # Expose the advertised banks to the verifier without extending them.
    yield 0, EV_NO_ACTION, {alg: bytes(size) for alg, size in sizes.items()}, first
    while off < len(log):
        pcr, etype, count = unpack("<III")
        if not 1 <= count <= len(sizes):
            raise ValueError("invalid event digest count")
        digests = {}
        for _ in range(count):
            alg = unpack("<H")[0]
            if alg not in sizes or alg in digests:
                raise ValueError("unknown or duplicate event algorithm")
            digests[alg] = take(sizes[alg])
        data = take(unpack("<I")[0])
        yield pcr, etype, digests, data


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


def verify(text, expects=(), banks=None):
    pcrs, log = parse_serial(text)
    events = list(parse_log(log))
    logged_banks = {ALGS[alg][0] for alg in events[0][2]}
    expected_banks = set(banks) if banks is not None else logged_banks
    if expected_banks != logged_banks or set(pcrs) != expected_banks:
        raise ValueError(f"bank mismatch: expected {sorted(expected_banks)}, "
                         f"log {sorted(logged_banks)}, readings {sorted(pcrs)}")
    replay = {}
    seen = set()
    separators = set()
    ok = True
    print(f"{len(events)} events, {len(log)} bytes")
    for pcr, etype, digests, data in events:
        name, text = describe(etype, data)
        seen.add(f"{name}@{pcr}")
        print(f"  PCR{pcr:<2} {name:34} {text}")
        if etype == EV_NO_ACTION:
            continue
        if {ALGS[alg][0] for alg in digests} != expected_banks:
            print(f"FAIL: PCR{pcr} event does not measure every bank")
            ok = False
        if etype == 0x04:
            # Conventional BIOS permits 0 or 0xffffffff (SeaBIOS uses
            # the latter); 1 denotes a measurement failure.
            if data not in (bytes(4), b"\xff" * 4) or any(
                digest != hashlib.new(ALGS[alg][0], data).digest()
                for alg, digest in digests.items()
            ):
                print(f"FAIL: PCR{pcr} error or malformed separator")
                ok = False
            separators.add(pcr)
        for alg, digest in digests.items():
            bank, size = ALGS[alg]
            old = replay.setdefault(bank, {}).get(pcr, bytes(size))
            replay[bank][pcr] = hashlib.new(bank, old + digest).digest()
    sizes = dict(ALGS.values())
    for bank, values in sorted(pcrs.items()):
        for i in range(10):
            if i not in values or len(values[i]) != sizes[bank]:
                print(f"FAIL: missing or malformed {bank} PCR{i} reading")
                ok = False
                continue
            want = replay.get(bank, {}).get(i, bytes(sizes[bank]))
            good = values[i] == want
            ok &= good
            print(f"{'ok  ' if good else 'FAIL'} {bank} PCR{i}: {values[i].hex()}"
                  + ("" if good else f" replay {want.hex()}"))
    if not set(range(8)) <= separators:
        print("FAIL: missing PCR 0-7 separators")
        ok = False
    for expect in expects:
        if expect not in seen:
            print(f"FAIL: no {expect} in the log")
            ok = False
    print("PASS" if ok else "FAIL")
    return ok


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("serial")
    ap.add_argument("--expect", action="append", default=[],
                    help="EVENT_NAME@PCR that must be in the log")
    ap.add_argument("--banks", help="required comma-separated PCR banks (default: Spec ID banks)")
    args = ap.parse_args()
    try:
        with open(args.serial, errors="replace") as serial:
            ok = verify(serial.read(), args.expect, args.banks.split(",") if args.banks else None)
    except (ValueError, struct.error) as error:
        print(f"FAIL: {error}")
        return 1
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
