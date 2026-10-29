//! Installed-DRAM inventory, produced by raminit and consumed by SMBIOS.
//!
//! Raminit runs in an earlier stage than the SMBIOS writer, so the inventory
//! crosses stages as a fixed little-endian record, like coreboot's
//! `CBMEM_ID_MEMINFO`. Field encodings follow SMBIOS Type 16/17.

use zerocopy::little_endian::{U16, U32};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};

/// Upper bound on described slots; every supported controller has at most 4.
pub const MAX_MEMORY_DEVICES: usize = 4;

const MAGIC: [u8; 8] = *b"FSTMEM01";

/// A populated device whose capacity could not be established. This must
/// not be reported as an empty socket (SMBIOS Type 17 size = 0xffff).
pub const SIZE_UNKNOWN: u32 = u32::MAX;
/// SMBIOS Type 17 memory types.
pub const MEMORY_TYPE_DDR2: u8 = 0x13;
pub const MEMORY_TYPE_DDR4: u8 = 0x1a;
/// SMBIOS Type 17 form factors.
pub const FORM_FACTOR_UNKNOWN: u8 = 0x02;
pub const FORM_FACTOR_DIMM: u8 = 0x09;
pub const FORM_FACTOR_SODIMM: u8 = 0x0d;
/// SMBIOS Type 17 type-detail bits.
pub const TYPE_DETAIL_UNKNOWN: u16 = 1 << 2;
pub const TYPE_DETAIL_SYNCHRONOUS: u16 = 1 << 7;
pub const TYPE_DETAIL_REGISTERED: u16 = 1 << 13;
pub const TYPE_DETAIL_UNBUFFERED: u16 = 1 << 14;
pub const TYPE_DETAIL_LRDIMM: u16 = 1 << 15;
/// SMBIOS Type 16 error correction: none.
pub const ECC_NONE: u8 = 0x03;

/// One wired memory slot. `size_mib == 0` marks an empty slot;
/// [`SIZE_UNKNOWN`] marks a present device with unknown capacity. This
/// record can also be used in a slice without the small [`MemoryInfo`] ABI.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, FromBytes, IntoBytes, Immutable, KnownLayout)]
pub struct MemoryDevice {
    pub size_mib: U32,
    /// Speed raminit programmed, in MT/s.
    pub configured_mts: U16,
    /// Fastest speed the module supports, in MT/s.
    pub max_mts: U16,
    pub voltage_mv: U16,
    /// Data and total (data + ECC) width in bits.
    pub data_width: U16,
    pub total_width: U16,
    pub type_detail: U16,
    /// JEP106 manufacturer: continuation-code count and id byte (with parity).
    pub jedec_bank: u8,
    pub jedec_id: u8,
    pub memory_type: u8,
    pub form_factor: u8,
    pub ranks: u8,
    pub channel: u8,
    /// Slot index within the channel.
    pub slot: u8,
    pub serial: [u8; 4],
    /// Module part number as stored in SPD, including space padding.
    pub part_number: [u8; 20],
}

impl MemoryDevice {
    /// A wired slot without a module.
    pub fn empty(channel: u8, slot: u8) -> Self {
        Self {
            channel,
            slot,
            ..Self::default()
        }
    }

    pub fn is_populated(&self) -> bool {
        self.size_mib.get() != 0
    }

    /// A present module whose SPD cannot be decoded reliably.
    pub fn unknown(channel: u8, slot: u8) -> Self {
        Self {
            size_mib: SIZE_UNKNOWN.into(),
            data_width: 0xffff.into(),
            total_width: 0xffff.into(),
            memory_type: 0x02,
            form_factor: FORM_FACTOR_UNKNOWN,
            type_detail: TYPE_DETAIL_UNKNOWN.into(),
            ..Self::empty(channel, slot)
        }
    }

    /// Hex representation of the SPD serial, without inventing a serial for
    /// empty, zero-filled or erased identity fields.
    pub fn serial_number(&self) -> Option<heapless::String<8>> {
        use core::fmt::Write;
        if !self.is_populated() || self.serial == [0; 4] || self.serial == [0xff; 4] {
            return None;
        }
        let mut text = heapless::String::new();
        for byte in self.serial {
            write!(text, "{byte:02x}").ok()?;
        }
        Some(text)
    }

    /// Part number without SPD padding; `None` for non-ASCII content.
    pub fn part_number(&self) -> Option<&str> {
        let bytes = &self.part_number;
        let end = bytes
            .iter()
            .rposition(|byte| !matches!(byte, b' ' | 0))
            .map_or(0, |last| last + 1);
        let trimmed = &bytes[..end];
        trimmed
            .iter()
            .all(|byte| (0x20..0x7f).contains(byte))
            .then(|| core::str::from_utf8(trimmed).ok())
            .flatten()
    }

    /// Manufacturer name for well-known JEP106 ids, from coreboot's table.
    pub fn manufacturer(&self) -> Option<&'static str> {
        Some(match (self.jedec_bank, self.jedec_id) {
            (0, 0x2c) => "Micron",
            (0, 0xad) => "Hynix",
            (0, 0xc1) => "Infineon",
            (0, 0xce) => "Samsung",
            (1, 0x4f) => "Transcend",
            (1, 0x98) => "Kingston",
            (2, 0x9e) => "Corsair",
            (2, 0xfe) => "Elpida",
            (3, 0x0b) => "Nanya",
            (4, 0x43) => "Ramaxel",
            (4, 0xb0) => "OCZ",
            (4, 0xcd) => "GSkill",
            (5, 0x9b) => "Crucial",
            (6, 0x34) => "Super Talent",
            _ => return None,
        })
    }
}

/// Inventory of one memory controller's slots.
#[repr(C)]
#[derive(Debug, Clone, Copy, FromBytes, IntoBytes, Immutable, KnownLayout)]
pub struct MemoryInfo {
    magic: [u8; 8],
    /// Largest total capacity the controller supports, in MiB.
    pub max_capacity_mib: U32,
    /// SMBIOS Type 16 error-correction encoding.
    pub ecc: u8,
    slots: u8,
    reserved: [u8; 2],
    devices: [MemoryDevice; MAX_MEMORY_DEVICES],
}

impl MemoryInfo {
    pub fn new(max_capacity_mib: u32) -> Self {
        Self {
            magic: MAGIC,
            max_capacity_mib: max_capacity_mib.into(),
            ecc: ECC_NONE,
            slots: 0,
            reserved: [0; 2],
            devices: [MemoryDevice::default(); MAX_MEMORY_DEVICES],
        }
    }

    /// Describe the next wired slot. Returns `false` when the record is full.
    pub fn push(&mut self, device: MemoryDevice) -> bool {
        let Some(entry) = self.devices.get_mut(usize::from(self.slots)) else {
            return false;
        };
        *entry = device;
        self.slots += 1;
        true
    }

    /// All wired slots, populated or not.
    pub fn devices(&self) -> &[MemoryDevice] {
        &self.devices[..usize::from(self.slots)]
    }

    /// Validate a record published by an earlier stage.
    pub fn from_bytes(bytes: &[u8]) -> Option<&Self> {
        let (info, _) = Self::ref_from_prefix(bytes).ok()?;
        (info.magic == MAGIC && usize::from(info.slots) <= MAX_MEMORY_DEVICES).then_some(info)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_rejects_foreign_bytes() {
        let mut info = MemoryInfo::new(4096);
        let mut dimm = MemoryDevice {
            size_mib: 2048.into(),
            ..MemoryDevice::empty(0, 0)
        };
        dimm.part_number[..8].copy_from_slice(b"M4 70T56");
        assert!(info.push(dimm));
        assert!(info.push(MemoryDevice::empty(1, 0)));

        let parsed = MemoryInfo::from_bytes(info.as_bytes()).unwrap();
        assert_eq!(parsed.devices().len(), 2);
        assert!(parsed.devices()[0].is_populated());
        assert!(!parsed.devices()[1].is_populated());
        assert_eq!(parsed.devices()[0].part_number(), Some("M4 70T56"));

        assert!(MemoryInfo::from_bytes(&[0; size_of::<MemoryInfo>()]).is_none());
        let full = (0..MAX_MEMORY_DEVICES).all(|_| info.push(MemoryDevice::default()));
        assert!(!full);
    }

    #[test]
    fn unknown_is_present_and_serials_are_not_fabricated() {
        let mut device = MemoryDevice::unknown(7, 1);
        assert!(device.is_populated());
        assert_eq!(device.size_mib.get(), SIZE_UNKNOWN);
        assert_eq!(device.serial_number(), None);
        device.serial = [0xff; 4];
        assert_eq!(device.serial_number(), None);
        device.serial = [0x12, 0, 0xab, 0xcd];
        assert_eq!(device.serial_number().as_deref(), Some("1200abcd"));
        device.size_mib = 0.into();
        assert_eq!(device.serial_number(), None);
    }

    #[test]
    fn part_numbers_must_be_printable() {
        let mut dimm = MemoryDevice::default();
        assert_eq!(dimm.part_number(), Some(""));
        dimm.part_number[0] = 0xff;
        assert_eq!(dimm.part_number(), None);
    }
}
