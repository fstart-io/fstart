//! PCI capability-list traversal helpers.

use crate::{PciBdf, PciConfigAccess, PCI_CAPABILITIES_PTR, PCI_STATUS};

const PCI_STATUS_CAP_LIST: u16 = 1 << 4;
const CAP_ID_OFFSET: u16 = 0x00;
const CAP_NEXT_OFFSET: u16 = 0x01;
const CAP_PTR_MASK: u8 = 0xfc;
const MAX_CAPABILITIES: usize = 48;

/// Find a conventional PCI capability by ID.
///
/// The returned offset is the capability structure base in config space. This
/// helper performs raw byte reads because capabilities are offset-linked and do
/// not fit a fixed config-space overlay.
pub fn find_capability<A: PciConfigAccess>(
    access: &A,
    bdf: PciBdf,
    cap_id: u8,
) -> Result<Option<u16>, A::Error> {
    if (access.read16(bdf, PCI_STATUS)? & PCI_STATUS_CAP_LIST) == 0 {
        return Ok(None);
    }

    let mut ptr = access.read8(bdf, PCI_CAPABILITIES_PTR)? & CAP_PTR_MASK;
    let mut remaining = MAX_CAPABILITIES;

    while ptr >= 0x40 && remaining != 0 {
        let off = ptr as u16;
        if access.read8(bdf, off + CAP_ID_OFFSET)? == cap_id {
            return Ok(Some(off));
        }
        ptr = access.read8(bdf, off + CAP_NEXT_OFFSET)? & CAP_PTR_MASK;
        remaining -= 1;
    }

    Ok(None)
}
