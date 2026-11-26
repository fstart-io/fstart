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

// DOE Extended Capability register offsets (relative to capability base).
const REG_CAP: u16 = 0x04;
const REG_CTRL: u16 = 0x08;
const REG_STATUS: u16 = 0x0C;
const REG_WRITE_MBX: u16 = 0x10;
const REG_READ_MBX: u16 = 0x14;

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

/// Walk the PCIe Extended Capability list starting at `0x100` to find the
/// first DOE capability (`0x002E`). Returns its config-space byte offset.
pub fn find_doe_capability<A: ConfigRegionAccess>(access: &A, addr: PciAddress) -> Option<u16> {
    let mut offset: u16 = 0x100;
    for _ in 0..64 {
        let header = unsafe { access.read(addr, offset) };
        if header == 0 || header == 0xFFFF_FFFF {
            return None;
        }
        let cap_id = (header & 0xFFFF) as u16;
        if cap_id == EXT_CAP_ID_DOE {
            return Some(offset);
        }
        let next = ((header >> 20) & 0x0FFC) as u16;
        if next < 0x100 || next == offset {
            return None;
        }
        offset = next;
    }
    None
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
    let vendor_id = protocol.vendor_id;
    let data_object_type = protocol.data_object_type;
    // Check DOE capability register is readable (non-all-ones).
    let _cap = unsafe { access.read(addr, cap_offset + REG_CAP) };

    // Wait for DOE Busy == 0, aborting if Error is latched.
    for _ in 0..max_polls {
        let st = unsafe { access.read(addr, cap_offset + REG_STATUS) };
        if (st & STATUS_ERROR) != 0 {
            unsafe { access.write(addr, cap_offset + REG_CTRL, CTRL_ABORT) };
            continue;
        }
        if (st & STATUS_BUSY) == 0 {
            break;
        }
    }
    let st = unsafe { access.read(addr, cap_offset + REG_STATUS) };
    if (st & STATUS_BUSY) != 0 {
        return Err(DoeError::Timeout);
    }

    let payload_dws = payload.len().div_ceil(4);
    let total_dws = 2 + payload_dws;
    if total_dws > 0x0003_FFFF {
        return Err(DoeError::InvalidLength);
    }

    // Write 2-DW DOE header: DW0 = vendor_id | (data_object_type << 16), DW1 = total_dws.
    let dw0 = u32::from(vendor_id) | (u32::from(data_object_type) << 16);
    let dw1 = (total_dws as u32) & 0x0003_FFFF;
    unsafe {
        access.write(addr, cap_offset + REG_WRITE_MBX, dw0);
        access.write(addr, cap_offset + REG_WRITE_MBX, dw1);
    }

    // Write payload DWs (little-endian, trailing bytes zero-padded).
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

    // Poll for Data Object Ready.
    let mut ready = false;
    for _ in 0..max_polls {
        let st = unsafe { access.read(addr, cap_offset + REG_STATUS) };
        if (st & STATUS_ERROR) != 0 {
            unsafe { access.write(addr, cap_offset + REG_CTRL, CTRL_ABORT) };
            return Err(DoeError::DeviceError);
        }
        if (st & STATUS_DATA_READY) != 0 {
            ready = true;
            break;
        }
    }
    if !ready {
        return Err(DoeError::Timeout);
    }

    // Read 2-DW DOE response header (each read must be followed by a write to
    // REG_READ_MBX to advance the mailbox pointer per PCIe §6.30.2).
    let r_dw0 = read_mbx_dw(access, addr, cap_offset);
    let r_dw1 = read_mbx_dw(access, addr, cap_offset);

    let r_vid = (r_dw0 & 0xFFFF) as u16;
    let r_type = ((r_dw0 >> 16) & 0xFF) as u8;
    let r_total_dws = (r_dw1 & 0x0003_FFFF) as usize;
    if r_total_dws < 2 {
        return Err(DoeError::InvalidLength);
    }
    let r_payload_dws = r_total_dws - 2;
    let r_bytes = r_payload_dws * 4;

    if r_vid != vendor_id || r_type != data_object_type {
        // Drain remaining DWs before returning error.
        for _ in 0..r_payload_dws {
            let _ = read_mbx_dw(access, addr, cap_offset);
        }
        return Err(DoeError::ProtocolMismatch);
    }
    if resp_buf.len() < r_bytes {
        for _ in 0..r_payload_dws {
            let _ = read_mbx_dw(access, addr, cap_offset);
        }
        return Err(DoeError::BufferTooSmall);
    }
    for i in 0..r_payload_dws {
        let dw = read_mbx_dw(access, addr, cap_offset);
        resp_buf[i * 4..i * 4 + 4].copy_from_slice(&dw.to_le_bytes());
    }
    Ok(r_bytes)
}

#[inline]
fn read_mbx_dw<A: ConfigRegionAccess>(access: &A, addr: PciAddress, cap_offset: u16) -> u32 {
    let dw = unsafe { access.read(addr, cap_offset + REG_READ_MBX) };
    // Writing any value to Read Data Mailbox advances to the next DW.
    unsafe { access.write(addr, cap_offset + REG_READ_MBX, 0) };
    dw
}
