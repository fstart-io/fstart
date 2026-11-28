//! PCIe Data Object Exchange (DOE) Extended Capability (PCIe 6.0 §6.30 / §7.9.24).
//!
//! Operates directly on `pci_types::ConfigRegionAccess` with no wrapper trait.
//! Supports DOE Discovery (`0x0001/0x00`), CMA-SPDM (`0x0001/0x01`), and
//! Secured CMA-SPDM (`0x0001/0x02`).

use pci_types::{ConfigRegionAccess, PciAddress};
use tock_registers::register_bitfields;
use zerocopy::little_endian::{U16 as LeU16, U32 as LeU32};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

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

register_bitfields! [u32,
    /// PCIe Extended Capability Header (offset +0x00, 32-bit).
    PcieExtCapHeader [
        CAP_ID OFFSET(0) NUMBITS(16) [],
        VERSION OFFSET(16) NUMBITS(4) [],
        NEXT_OFFSET OFFSET(20) NUMBITS(12) [],
    ],
    /// PCIe DOE Control Register (offset +0x08, 32-bit).
    DoeControl [
        ABORT OFFSET(0) NUMBITS(1) [],
        INT_ENABLE OFFSET(1) NUMBITS(1) [],
        GO OFFSET(31) NUMBITS(1) [],
    ],
    /// PCIe DOE Status Register (offset +0x0C, 32-bit).
    DoeStatus [
        BUSY OFFSET(0) NUMBITS(1) [],
        INT_STATUS OFFSET(1) NUMBITS(1) [],
        ERROR OFFSET(2) NUMBITS(1) [],
        DATA_READY OFFSET(31) NUMBITS(1) [],
    ],
    /// PCIe DOE Data Object Header DW1 (length in DWs, 18-bit; 0 encodes 2^18).
    DoeLengthWord [
        LENGTH OFFSET(0) NUMBITS(18) [],
    ],
];

/// Wire-format 8-byte PCIe DOE Data Object Header (PCIe 6.0 §6.30.1).
#[derive(Debug, Clone, Copy, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
pub struct DoeObjectHeader {
    pub vendor_id: LeU16,
    pub object_type: u8,
    pub reserved: u8,
    pub length_word: LeU32,
}

impl DoeObjectHeader {
    /// Construct a DOE header for `protocol` and `total_dws` (including the 2 header DWs).
    pub fn new(protocol: DoeProtocol, total_dws: usize) -> Result<Self, DoeError> {
        if !(2..=MAX_DOE_DWS).contains(&total_dws) {
            return Err(DoeError::InvalidLength);
        }
        let encoded = if total_dws == MAX_DOE_DWS {
            0
        } else {
            total_dws as u32
        };
        Ok(Self {
            vendor_id: LeU16::new(protocol.vendor_id),
            object_type: protocol.data_object_type,
            reserved: 0,
            length_word: LeU32::new((DoeLengthWord::LENGTH.val(encoded)).value),
        })
    }

    /// Decode total length in DWs (interpreting `0` as `2^18`).
    pub fn total_dws(&self) -> usize {
        let raw = DoeLengthWord::LENGTH.read(self.length_word.get()) as usize;
        if raw == 0 { MAX_DOE_DWS } else { raw }
    }

    pub fn to_dwords(self) -> [u32; 2] {
        let words: [LeU32; 2] = zerocopy::transmute!(self);
        [words[0].get(), words[1].get()]
    }

    pub fn from_dwords(dw0: u32, dw1: u32) -> Self {
        let words = [LeU32::new(dw0), LeU32::new(dw1)];
        zerocopy::transmute!(words)
    }
}

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
        if PcieExtCapHeader::CAP_ID.read(header) as u16 == EXT_CAP_ID_DOE {
            return Some(offset);
        }
        let raw_next = PcieExtCapHeader::NEXT_OFFSET.read(header) as u16;
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
    unsafe { access.write(addr, cap_offset + REG_CTRL, DoeControl::ABORT::SET.value) };
    for _ in 0..max_polls {
        let st = unsafe { access.read(addr, cap_offset + REG_STATUS) };
        if !DoeStatus::BUSY.is_set(st) && !DoeStatus::ERROR.is_set(st) {
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

    let _cap = unsafe { access.read(addr, cap_offset + REG_CAP) };

    // Ensure mailbox is clean (neither Busy, Error, nor stale Data Object Ready).
    let st = unsafe { access.read(addr, cap_offset + REG_STATUS) };
    if DoeStatus::ERROR.is_set(st) || DoeStatus::DATA_READY.is_set(st) {
        abort_and_wait(access, addr, cap_offset, max_polls)?;
    } else if DoeStatus::BUSY.is_set(st) {
        let mut ready = false;
        for _ in 0..max_polls {
            let s = unsafe { access.read(addr, cap_offset + REG_STATUS) };
            if DoeStatus::ERROR.is_set(s) || DoeStatus::DATA_READY.is_set(s) {
                abort_and_wait(access, addr, cap_offset, max_polls)?;
                ready = true;
                break;
            }
            if !DoeStatus::BUSY.is_set(s) {
                ready = true;
                break;
            }
        }
        if !ready {
            return Err(DoeError::Timeout);
        }
    }

    let payload_dws = payload.len().div_ceil(4);
    let tx_hdr = DoeObjectHeader::new(protocol, 2 + payload_dws)?;
    let [dw0, dw1] = tx_hdr.to_dwords();
    unsafe {
        access.write(addr, cap_offset + REG_WRITE_MBX, dw0);
        access.write(addr, cap_offset + REG_WRITE_MBX, dw1);
    }

    for chunk in payload.chunks(4) {
        let mut word = [0u8; 4];
        word[..chunk.len()].copy_from_slice(chunk);
        let le_word = LeU32::read_from_bytes(&word).unwrap();
        unsafe {
            access.write(addr, cap_offset + REG_WRITE_MBX, le_word.get());
        }
    }

    // Trigger DOE Go.
    unsafe {
        access.write(addr, cap_offset + REG_CTRL, DoeControl::GO::SET.value);
    }

    // Poll for Data Object Ready; abort mailbox on timeout or device error.
    let mut ready = false;
    for _ in 0..max_polls {
        let st = unsafe { access.read(addr, cap_offset + REG_STATUS) };
        if DoeStatus::ERROR.is_set(st) {
            let _ = abort_and_wait(access, addr, cap_offset, max_polls);
            return Err(DoeError::DeviceError);
        }
        if DoeStatus::DATA_READY.is_set(st) {
            ready = true;
            break;
        }
    }
    if !ready {
        let _ = abort_and_wait(access, addr, cap_offset, max_polls);
        return Err(DoeError::Timeout);
    }

    let r_hdr = DoeObjectHeader::from_dwords(
        read_mbx_dw(access, addr, cap_offset),
        read_mbx_dw(access, addr, cap_offset),
    );
    let r_total_dws = r_hdr.total_dws();
    if r_total_dws < 2 {
        let _ = abort_and_wait(access, addr, cap_offset, max_polls);
        return Err(DoeError::InvalidLength);
    }
    let r_payload_dws = r_total_dws - 2;
    let r_bytes = r_payload_dws * 4;

    if r_hdr.vendor_id.get() != protocol.vendor_id || r_hdr.object_type != protocol.data_object_type
    {
        let _ = abort_and_wait(access, addr, cap_offset, max_polls);
        return Err(DoeError::ProtocolMismatch);
    }
    if resp_buf.len() < r_bytes {
        let _ = abort_and_wait(access, addr, cap_offset, max_polls);
        return Err(DoeError::BufferTooSmall);
    }
    for i in 0..r_payload_dws {
        let dw = LeU32::new(read_mbx_dw(access, addr, cap_offset));
        dw.write_to_prefix(&mut resp_buf[i * 4..])
            .map_err(|_| DoeError::BufferTooSmall)?;
    }

    // Verify the final read acknowledgment cleared Data Object Ready without Error.
    let final_st = unsafe { access.read(addr, cap_offset + REG_STATUS) };
    if DoeStatus::ERROR.is_set(final_st) || DoeStatus::DATA_READY.is_set(final_st) {
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
