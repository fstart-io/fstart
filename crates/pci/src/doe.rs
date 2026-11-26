//! PCIe Data Object Exchange (DOE) Extended Capability (PCIe 6.0 §6.30 / §7.9.24).
//!
//! Operates directly on `pci_types::ConfigRegionAccess` with no wrapper trait.
//! Supports DOE Discovery (`0x0001/0x00`), CMA-SPDM (`0x0001/0x01`), and
//! Secured CMA-SPDM (`0x0001/0x02`).

use pci_types::{ConfigRegionAccess, PciAddress};

/// PCIe Extended Capability ID for Data Object Exchange (DOE).
pub const EXT_CAP_ID_DOE: u16 = 0x002E;

/// PCI-SIG Vendor ID for standard DOE protocols.
pub const VENDOR_ID_PCI_SIG: u16 = 0x0001;

/// Standard PCI-SIG DOE data object types.
pub const DOE_TYPE_DISCOVERY: u8 = 0x00;
pub const DOE_TYPE_CMA_SPDM: u8 = 0x01;
pub const DOE_TYPE_SECURED_CMA_SPDM: u8 = 0x02;

/// Maximum DOE data object length in DWs (`2^18`, encoded as `0` on the wire).
const MAX_DOE_DWS: usize = 1 << 18;

// DOE Extended Capability register offsets (relative to capability base).
const REG_CAP: u16 = 0x04;
const REG_CTRL: u16 = 0x08;
const REG_STATUS: u16 = 0x0C;
const REG_WRITE_MBX: u16 = 0x10;
const REG_READ_MBX: u16 = 0x14;
const DOE_CAP_SPAN: u16 = 0x18;

const CTRL_ABORT: u32 = 1 << 0;
const CTRL_GO: u32 = 1 << 31;

const STATUS_BUSY: u32 = 1 << 0;
const STATUS_ERROR: u32 = 1 << 2;
const STATUS_DATA_READY: u32 = 1 << 31;

/// Errors returned by PCIe DOE mailbox operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DoeError {
    NotFound,
    Timeout,
    DeviceError,
    InvalidLength,
    BufferTooSmall,
    ProtocolMismatch,
}

/// PCIe DOE protocol identifier `(vendor_id, data_object_type)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DoeProtocol {
    pub vendor_id: u16,
    pub data_object_type: u8,
}

impl DoeProtocol {
    pub const DISCOVERY: Self = Self {
        vendor_id: VENDOR_ID_PCI_SIG,
        data_object_type: DOE_TYPE_DISCOVERY,
    };
    pub const CMA_SPDM: Self = Self {
        vendor_id: VENDOR_ID_PCI_SIG,
        data_object_type: DOE_TYPE_CMA_SPDM,
    };
    pub const SECURED_CMA_SPDM: Self = Self {
        vendor_id: VENDOR_ID_PCI_SIG,
        data_object_type: DOE_TYPE_SECURED_CMA_SPDM,
    };
}

/// Validate that a DOE capability base offset is DW-aligned and its entire
/// 0x18-byte register block fits inside the 4 KiB PCIe extended config space.
const fn valid_doe_offset(offset: u16) -> bool {
    offset >= 0x100 && (offset & 0x03) == 0 && offset <= 0x1000 - DOE_CAP_SPAN
}

/// Walk the PCIe Extended Capability list starting at `0x100` to find the
/// first DOE capability (`0x002E`). Returns its config-space byte offset.
pub fn find_doe_capability<A: ConfigRegionAccess>(access: &A, addr: PciAddress) -> Option<u16> {
    let mut offset: u16 = 0x100;
    for _ in 0..960 {
        if !valid_doe_offset(offset) {
            return None;
        }
        let header = unsafe { access.read(addr, offset) };
        if header == 0 || header == 0xFFFF_FFFF {
            return None;
        }
        let cap_id = (header & 0xFFFF) as u16;
        if cap_id == EXT_CAP_ID_DOE {
            return Some(offset);
        }
        let raw_next = ((header >> 20) & 0x0FFF) as u16;
        if raw_next == 0 || (raw_next & 0x03) != 0 || raw_next < 0x100 || raw_next == offset {
            return None;
        }
        offset = raw_next;
    }
    None
}

/// Issue DOE Abort once and wait up to `max_polls` for `Busy == 0 && Error == 0`.
fn abort_and_wait<A: ConfigRegionAccess>(
    access: &A,
    addr: PciAddress,
    cap_offset: u16,
    max_polls: u32,
) -> Result<(), DoeError> {
    unsafe { access.write(addr, cap_offset + REG_CTRL, CTRL_ABORT) };
    for _ in 0..max_polls {
        let st = unsafe { access.read(addr, cap_offset + REG_STATUS) };
        if (st & (STATUS_BUSY | STATUS_ERROR)) == 0 {
            return Ok(());
        }
    }
    Err(DoeError::Timeout)
}

/// Send one DOE data object (`protocol`, `payload`) and read the response
/// payload into `resp_buf`.
///
/// `payload` is automatically zero-padded to a 4-byte DW boundary on write;
/// returns the response payload byte length (multiple of 4, excluding the
/// 8-byte DOE header).
pub fn doe_exchange<A: ConfigRegionAccess>(
    access: &A,
    addr: PciAddress,
    cap_offset: u16,
    protocol: DoeProtocol,
    payload: &[u8],
    resp_buf: &mut [u8],
    max_polls: u32,
) -> Result<usize, DoeError> {
    if !valid_doe_offset(cap_offset) {
        return Err(DoeError::NotFound);
    }
    let vendor_id = protocol.vendor_id;
    let data_object_type = protocol.data_object_type;

    let _cap = unsafe { access.read(addr, cap_offset + REG_CAP) };

    // Ensure mailbox is clean (neither Busy nor Error latched).
    let st = unsafe { access.read(addr, cap_offset + REG_STATUS) };
    if (st & STATUS_ERROR) != 0 {
        abort_and_wait(access, addr, cap_offset, max_polls)?;
    } else if (st & STATUS_BUSY) != 0 {
        let mut ready = false;
        for _ in 0..max_polls {
            let s = unsafe { access.read(addr, cap_offset + REG_STATUS) };
            if (s & STATUS_ERROR) != 0 {
                abort_and_wait(access, addr, cap_offset, max_polls)?;
                ready = true;
                break;
            }
            if (s & STATUS_BUSY) == 0 {
                ready = true;
                break;
            }
        }
        if !ready {
            return Err(DoeError::Timeout);
        }
    }

    let payload_dws = payload.len().div_ceil(4);
    let total_dws = 2 + payload_dws;
    if total_dws > MAX_DOE_DWS {
        return Err(DoeError::InvalidLength);
    }

    // Write 2-DW DOE header: DW0 = vendor_id | (data_object_type << 16),
    // DW1 = total_dws (with 2^18 encoded as 0 per PCIe 6.0 §6.30.1).
    let dw0 = u32::from(vendor_id) | (u32::from(data_object_type) << 16);
    let dw1 = if total_dws == MAX_DOE_DWS {
        0
    } else {
        (total_dws as u32) & 0x0003_FFFF
    };
    unsafe {
        access.write(addr, cap_offset + REG_WRITE_MBX, dw0);
        access.write(addr, cap_offset + REG_WRITE_MBX, dw1);
    }

    for chunk in payload.chunks(4) {
        let mut word = [0u8; 4];
        word[..chunk.len()].copy_from_slice(chunk);
        unsafe {
            access.write(addr, cap_offset + REG_WRITE_MBX, u32::from_le_bytes(word));
        }
    }

    // Trigger DOE Go.
    unsafe {
        access.write(addr, cap_offset + REG_CTRL, CTRL_GO);
    }

    // Poll for Data Object Ready; abort mailbox on timeout or device error.
    let mut ready = false;
    for _ in 0..max_polls {
        let st = unsafe { access.read(addr, cap_offset + REG_STATUS) };
        if (st & STATUS_ERROR) != 0 {
            let _ = abort_and_wait(access, addr, cap_offset, max_polls);
            return Err(DoeError::DeviceError);
        }
        if (st & STATUS_DATA_READY) != 0 {
            ready = true;
            break;
        }
    }
    if !ready {
        let _ = abort_and_wait(access, addr, cap_offset, max_polls);
        return Err(DoeError::Timeout);
    }

    let r_dw0 = read_mbx_dw(access, addr, cap_offset);
    let r_dw1 = read_mbx_dw(access, addr, cap_offset);

    let r_vid = (r_dw0 & 0xFFFF) as u16;
    let r_type = ((r_dw0 >> 16) & 0xFF) as u8;
    let raw_len = (r_dw1 & 0x0003_FFFF) as usize;
    let r_total_dws = if raw_len == 0 { MAX_DOE_DWS } else { raw_len };
    if r_total_dws < 2 {
        let _ = abort_and_wait(access, addr, cap_offset, max_polls);
        return Err(DoeError::InvalidLength);
    }
    let r_payload_dws = r_total_dws - 2;
    let r_bytes = r_payload_dws * 4;

    if r_vid != vendor_id || r_type != data_object_type {
        let _ = abort_and_wait(access, addr, cap_offset, max_polls);
        return Err(DoeError::ProtocolMismatch);
    }
    if resp_buf.len() < r_bytes {
        let _ = abort_and_wait(access, addr, cap_offset, max_polls);
        return Err(DoeError::BufferTooSmall);
    }
    for i in 0..r_payload_dws {
        let dw = read_mbx_dw(access, addr, cap_offset);
        resp_buf[i * 4..i * 4 + 4].copy_from_slice(&dw.to_le_bytes());
    }

    // Verify the final read acknowledgment cleared Data Object Ready without Error.
    let final_st = unsafe { access.read(addr, cap_offset + REG_STATUS) };
    if (final_st & (STATUS_ERROR | STATUS_DATA_READY)) != 0 {
        let _ = abort_and_wait(access, addr, cap_offset, max_polls);
        return Err(DoeError::DeviceError);
    }
    Ok(r_bytes)
}

#[inline]
fn read_mbx_dw<A: ConfigRegionAccess>(access: &A, addr: PciAddress, cap_offset: u16) -> u32 {
    let dw = unsafe { access.read(addr, cap_offset + REG_READ_MBX) };
    unsafe { access.write(addr, cap_offset + REG_READ_MBX, 0) };
    dw
}
