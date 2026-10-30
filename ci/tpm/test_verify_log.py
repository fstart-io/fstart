#!/usr/bin/env python3
"""Regression tests for the measured-boot integration test's pass/fail gate."""
import base64
import contextlib
import hashlib
import importlib.util
import io
import struct
import unittest
from pathlib import Path

SPEC = importlib.util.spec_from_file_location("verify_log", Path(__file__).with_name("verify-log.py"))
assert SPEC is not None and SPEC.loader is not None
verifier = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(verifier)
ALGS = {"sha1": (4, 20), "sha256": (11, 32), "sha384": (12, 48), "sha512": (13, 64)}


def fixture(banks=("sha1",), agile=False, separator=0, omit_separator=None,
            omit_event=None, omit_digest=None):
    """Build independent wire-format fixtures and their PCR readings."""
    events = [(0, 8, b"version"), (0, 7, b"firmware"),
              (0, 0x13 if agile else 1, b"payload")]
    events += [(i, 4, struct.pack("<I", separator)) for i in range(8) if i != omit_separator]
    events += [(4, 0x0d, b"boot sector")]
    events = [event for event in events if event[1] != omit_event]
    readings = {bank: {i: bytes(ALGS[bank][1]) for i in range(10)} for bank in banks}
    if agile:
        spec = b"Spec ID Event03\0" + struct.pack("<I4BI", 0, 0, 2, 106, 2, len(banks))
        spec += b"".join(struct.pack("<HH", *ALGS[bank]) for bank in banks) + b"\0"
    else:
        spec = b"Spec ID Event00\0" + struct.pack("<I", 0) + bytes([2, 1, 1, 0, 0])
    log = struct.pack("<II20sI", 0, 3, bytes(20), len(spec)) + spec
    for pcr, etype, data in events:
        digests = {bank: hashlib.new(bank, data).digest() for bank in banks if bank != omit_digest}
        for bank, digest in digests.items():
            readings[bank][pcr] = hashlib.new(bank, readings[bank][pcr] + digest).digest()
        if agile:
            log += struct.pack("<III", pcr, etype, len(digests))
            log += b"".join(struct.pack("<H", ALGS[bank][0]) + digest
                            for bank, digest in digests.items())
            log += struct.pack("<I", len(data)) + data
        else:
            log += struct.pack("<II20sI", pcr, etype, digests["sha1"], len(data)) + data
    return readings, log


def serial(readings, log):
    text = "".join(f"TPMTEST pcr {bank} {i} {value.hex()}\n"
                   for bank, values in readings.items() for i, value in values.items())
    return text + "TPMTEST_LOG_BEGIN\n" + base64.b64encode(log).decode() + "\nTPMTEST_LOG_END\n"


class VerifyLogTests(unittest.TestCase):
    def verify(self, readings, log, banks=None, expects=()):
        with contextlib.redirect_stdout(io.StringIO()):
            return verifier.verify(serial(readings, log), expects, banks)

    def assert_bad(self, readings, log, banks=None, expects=()):
        try:
            result = self.verify(readings, log, banks, expects)
        except (ValueError, struct.error):
            return
        self.assertFalse(result)

    def test_complete_logs(self):
        for banks, agile in [(("sha1",), False), (("sha1", "sha256"), True),
                             (("sha256", "sha384"), True), (("sha512",), True)]:
            for separator in (0, 0xffffffff):
                with self.subTest(banks=banks, separator=separator):
                    readings, log = fixture(banks, agile, separator)
                    payload = "EV_POST_CODE2@0" if agile else "EV_POST_CODE@0"
                    self.assertTrue(self.verify(readings, log, banks,
                                                ("EV_S_CRTM_VERSION@0", "EV_S_CRTM_CONTENTS@0", payload)))

    def test_legacy_linux_readings(self):
        readings, log = fixture()
        text = "".join(f"TPMTEST pcrs PCR-{i:02}: " + value.hex(" ").upper() + "\r\n"
                       for i, value in readings["sha1"].items())
        text += serial({}, log)
        with contextlib.redirect_stdout(io.StringIO()):
            self.assertTrue(verifier.verify(text))

    def test_missing_pcr_reading(self):
        for pcr in range(10):
            readings, log = fixture()
            del readings["sha1"][pcr]
            self.assert_bad(readings, log)

    def test_original_false_positive_only_pcr9(self):
        readings, log = fixture()
        readings["sha1"] = {9: bytes(20)}
        self.assert_bad(readings, log, expects=("EV_S_CRTM_VERSION@0", "EV_S_CRTM_CONTENTS@0"))

    def test_missing_bank_readings(self):
        readings, log = fixture(("sha1", "sha256"), True)
        del readings["sha256"]
        self.assert_bad(readings, log)

    def test_wrong_expected_banks(self):
        readings, log = fixture(("sha256",), True)
        self.assert_bad(readings, log, banks=("sha256", "sha384"))

    def test_log_omits_measurements_in_a_bank(self):
        readings, log = fixture(("sha1", "sha256"), True, omit_digest="sha256")
        self.assert_bad(readings, log)

    def test_malformed_pcr_digest(self):
        readings, log = fixture()
        readings["sha1"][0] = bytes(19)
        self.assert_bad(readings, log)

    def test_replay_mismatch(self):
        readings, log = fixture()
        readings["sha1"][0] = bytes(20)
        self.assert_bad(readings, log)

    def test_missing_separator_even_with_matching_replay(self):
        for pcr in range(8):
            self.assert_bad(*fixture(omit_separator=pcr))

    def test_error_separators_even_with_matching_replay(self):
        self.assert_bad(*fixture(separator=1))
        self.assert_bad(*fixture(("sha1", "sha256"), True, separator=1))

    def test_separator_digest_must_match_its_value(self):
        readings, log = fixture()
        # First separator's data: change success to 0xffffffff but keep its
        # digest and PCR reading. Replay alone would still match.
        events = list(verifier.parse_log(log))
        offset = sum(32 + len(event[3]) for event in events[:5]) - 4
        log = log[:offset] + b"\xff" * 4 + log[offset + 4:]
        self.assert_bad(readings, log)

    def test_payload_event_required_even_with_matching_replay(self):
        readings, log = fixture(omit_event=1)
        self.assert_bad(readings, log, expects=("EV_POST_CODE@0",))

    def test_smm_event_does_not_prove_seabios_continued_measuring(self):
        readings, log = fixture(omit_event=0x0d)
        # Keep POST_CODE (also emitted for SMM), firmware and separators,
        # with matching PCRs. The SeaBIOS-origin IPL measurement is required.
        self.assert_bad(readings, log, expects=("EV_POST_CODE@0", "EV_IPL@4"))

    def test_truncated_log(self):
        for agile, banks in [(False, ("sha1",)), (True, ("sha1", "sha256"))]:
            readings, log = fixture(banks, agile)
            for end in (0, 10, 31, 40, len(log) - 1):
                self.assert_bad(readings, log[:end])
            self.assert_bad(readings, log + b"\x01")

    def test_invalid_base64(self):
        with self.assertRaises(ValueError):
            verifier.parse_serial("TPMTEST_LOG_BEGIN\n!!!!\nTPMTEST_LOG_END\n")


if __name__ == "__main__":
    unittest.main()
