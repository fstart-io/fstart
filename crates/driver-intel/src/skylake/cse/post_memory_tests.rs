// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;

#[test]
fn mbp_walk_skips_unknown_variable_length_items_before_capabilities() {
    // Six MBP dwords, two entries: unknown 3-dword item, then SKU caps.
    let response = [
        0xf0, 0x82, 0, 0, 6, 2, 0, 0, 5, 3, 3, 0, 0xaa, 0xaa, 0xaa, 0xaa, 0xbb, 0xbb, 0xbb, 0xbb,
        1, 2, 2, 0, 0x10, 0, 0, 0,
    ];
    assert_eq!(
        bios_payload(&response),
        Ok(BiosPayload {
            firmware_capabilities: Some(0x10)
        })
    );
}
#[test]
fn mbp_rejects_inconsistent_counts_lengths_and_ambiguous_capabilities() {
    let valid = [0xf0, 0x82, 0, 0, 3, 1, 0, 0, 1, 2, 2, 0, 0x10, 0, 0, 0];
    for (index, value) in [(10, 0), (10, 3), (5, 2), (4, 4)] {
        let mut response = valid;
        response[index] = value;
        assert_eq!(bios_payload(&response), Err(Error::InvalidBiosPayload));
    }
    assert_eq!(bios_payload(&valid[..12]), Err(Error::InvalidBiosPayload));
    let duplicate = [
        0xf0, 0x82, 0, 0, 5, 2, 0, 0, 1, 2, 2, 0, 0x10, 0, 0, 0, 1, 2, 2, 0, 0, 0, 0, 0,
    ];
    assert_eq!(bios_payload(&duplicate), Err(Error::InvalidBiosPayload));
}
#[test]
fn eop_requires_exact_ack_and_keeps_reset_distinct_from_continuation() {
    let mut response = [0xff, 0x8c, 0, 0, 0, 0, 0, 0];
    assert_eq!(
        EndOfPostAction::parse(&response),
        Ok(EndOfPostAction::Continue)
    );
    response[4] = 1;
    assert_eq!(
        EndOfPostAction::parse(&response),
        Ok(EndOfPostAction::GlobalReset)
    );
    for action in [2u32, 256, u32::MAX] {
        response[4..8].copy_from_slice(&action.to_le_bytes());
        assert_eq!(
            EndOfPostAction::parse(&response),
            Err(Error::InvalidEopAction(action))
        );
    }
    response = [0xff, 0x81, 0, 0, 0, 0, 0, 0]; // DID-style opcode is NOT an EOP ACK.
    assert_eq!(
        EndOfPostAction::parse(&response),
        Err(Error::ResponseHeader)
    );
    response = [0xff, 0x8c, 0, 3, 0, 0, 0, 0];
    assert_eq!(
        EndOfPostAction::parse(&response),
        Err(Error::FirmwareResult(3))
    );
    assert_eq!(
        EndOfPostAction::parse(&[]),
        Err(Error::ResponseSize {
            expected: 8,
            actual: 0
        })
    );
}
#[test]
fn sensitive_payload_is_erased_on_error_without_touching_neighboring_memory() {
    let mut bytes = [0x5au8; 16];
    let result: Result<(), ()> = {
        let payload = PayloadBuffer(&mut bytes[4..12]);
        payload.0.fill(0x77); // a received secret before an error return
        Err(())
    };
    assert!(result.is_err());
    assert_eq!(&bytes[4..12], &[0; 8]);
    assert_eq!(&bytes[..4], &[0x5a; 4]);
    assert_eq!(&bytes[12..], &[0x5a; 4]);
}
