//! The CXL DVSECs (CXL 3.1 §8.1): DVSEC IDs, the registers firmware reads
//! or programs, and the Register Locator.

use tock_registers::{LocalRegisterCopy, register_bitfields};

use crate::EcamDevice;

/// DVSEC IDs of vendor [`super::VENDOR`] (CXL 3.1 Table 8-2).
pub mod id {
    /// PCIe DVSEC for CXL Devices (a device's function 0).
    pub const DEVICE: u16 = 0;
    pub const NON_CXL_FUNCTION_MAP: u16 = 2;
    /// CXL Extensions DVSEC for Ports (root ports, switch ports).
    pub const PORT_EXTENSIONS: u16 = 3;
    pub const GPF_PORT: u16 = 4;
    pub const GPF_DEVICE: u16 = 5;
    /// PCIe DVSEC for Flex Bus Port (both ends of a link).
    pub const FLEX_BUS_PORT: u16 = 7;
    pub const REGISTER_LOCATOR: u16 = 8;
    pub const MLD: u16 = 9;
    pub const TEST: u16 = 0xa;
}

/// The bytes a DVSEC `id` has at least (CXL 2.0 lengths), headers included.
pub const fn min_bytes(id: u16) -> u16 {
    match id {
        id::DEVICE => 0x38,
        id::PORT_EXTENSIONS => 0x28,
        id::GPF_PORT | id::GPF_DEVICE => 0x10,
        id::FLEX_BUS_PORT => 0x14,
        id::REGISTER_LOCATOR => 0x0c,
        _ => 0x0c,
    }
}

/// DVSEC header 2 (+0x04): the DVSEC's length in bits 31:20.
pub const HEADER2: u16 = 0x04;

/// PCIe DVSEC for CXL Devices (ID 0): CXL Capability (+0x0A, 16-bit).
pub const DEVICE_CAPABILITY: u16 = 0x0a;
/// CXL Control (+0x0C, 16-bit).
pub const DEVICE_CONTROL: u16 = 0x0c;
/// CXL Status (+0x0E, 16-bit).
pub const DEVICE_STATUS: u16 = 0x0e;
/// CXL Lock (+0x14, 16-bit): bit 0 locks the control and range registers.
pub const DEVICE_LOCK: u16 = 0x14;

/// Range `n` (`0..2`) Size High (+0x18 + 0x10n, 32-bit).
pub const fn range_size_high(n: u16) -> u16 {
    0x18 + 0x10 * n
}
/// Range `n` Size Low (+0x1C + 0x10n, 32-bit), [`RangeSizeLow`].
pub const fn range_size_low(n: u16) -> u16 {
    0x1c + 0x10 * n
}
/// Range `n` Base High (+0x20 + 0x10n, 32-bit).
pub const fn range_base_high(n: u16) -> u16 {
    0x20 + 0x10 * n
}
/// Range `n` Base Low (+0x24 + 0x10n, 32-bit), [`RangeBaseLow`].
pub const fn range_base_low(n: u16) -> u16 {
    0x24 + 0x10 * n
}

/// Flex Bus Port DVSEC (ID 7): Flex Bus Port Capability (+0x0A, 16-bit).
pub const FLEX_BUS_CAPABILITY: u16 = 0x0a;
/// Flex Bus Port Control (+0x0C, 16-bit).
pub const FLEX_BUS_CONTROL: u16 = 0x0c;
/// Flex Bus Port Status (+0x0E, 16-bit): what the link negotiated.
pub const FLEX_BUS_STATUS: u16 = 0x0e;

/// Register Locator DVSEC (ID 8): first Register Block entry (+0x0C), 8
/// bytes each to the end of the DVSEC.
pub const REGISTER_BLOCKS: u16 = 0x0c;

register_bitfields![u16,
    /// PCIe DVSEC for CXL Devices +0x0A (16-bit), CXL Capability.
    pub DeviceCapability [
        CACHE_CAPABLE OFFSET(0) NUMBITS(1) [],
        IO_CAPABLE OFFSET(1) NUMBITS(1) [],
        MEM_CAPABLE OFFSET(2) NUMBITS(1) [],
        MEM_HWINIT_MODE OFFSET(3) NUMBITS(1) [],
        HDM_COUNT OFFSET(4) NUMBITS(2) [],
        CACHE_WRITEBACK_INVALIDATE OFFSET(6) NUMBITS(1) [],
        CXL_RESET OFFSET(7) NUMBITS(1) [],
        VIRAL OFFSET(14) NUMBITS(1) []
    ],
    /// PCIe DVSEC for CXL Devices +0x0C (16-bit), CXL Control.
    pub DeviceControl [
        CACHE_ENABLE OFFSET(0) NUMBITS(1) [],
        IO_ENABLE OFFSET(1) NUMBITS(1) [],
        MEM_ENABLE OFFSET(2) NUMBITS(1) [],
        CACHE_SF_COVERAGE OFFSET(3) NUMBITS(5) [],
        CACHE_SF_GRANULARITY OFFSET(8) NUMBITS(3) [],
        CACHE_CLEAN_EVICTION OFFSET(11) NUMBITS(1) [],
        VIRAL_ENABLE OFFSET(14) NUMBITS(1) []
    ],
    /// PCIe DVSEC for Flex Bus Port +0x0A/+0x0C/+0x0E (16-bit each):
    /// capability, control and status share their low bits.
    pub FlexBusPort [
        CACHE OFFSET(0) NUMBITS(1) [],
        IO OFFSET(1) NUMBITS(1) [],
        MEM OFFSET(2) NUMBITS(1) [],
        /// 68B flit and virtual hierarchy: a CXL 2.0 (or later) link, not
        /// a restricted CXL host's CXL 1.1 one.
        VH_68B_FLIT OFFSET(5) NUMBITS(1) [],
        MULTI_LOGICAL_DEVICE OFFSET(6) NUMBITS(1) []
    ]
];

register_bitfields![u32,
    /// PCIe DVSEC for CXL Devices +0x1C + 0x10n (32-bit), Range n Size Low.
    pub RangeSizeLow [
        MEMORY_INFO_VALID OFFSET(0) NUMBITS(1) [],
        MEMORY_ACTIVE OFFSET(1) NUMBITS(1) [],
        MEDIA_TYPE OFFSET(2) NUMBITS(3) [],
        MEMORY_CLASS OFFSET(5) NUMBITS(3) [],
        DESIRED_INTERLEAVE OFFSET(8) NUMBITS(5) [],
        MEMORY_ACTIVE_TIMEOUT OFFSET(13) NUMBITS(3) [],
        /// Size bits 31:28 (256 MiB granular).
        SIZE_LOW OFFSET(28) NUMBITS(4) []
    ],
    /// PCIe DVSEC for CXL Devices +0x24 + 0x10n (32-bit), Range n Base Low.
    pub RangeBaseLow [
        /// Base bits 31:28.
        BASE_LOW OFFSET(28) NUMBITS(4) []
    ],
    /// Register Locator entry, Register Offset Low (32-bit).
    pub RegisterOffsetLow [
        /// The BAR (index `0..6`) the block is in.
        BIR OFFSET(0) NUMBITS(3) [],
        BLOCK_ID OFFSET(8) NUMBITS(8) [
            Empty = 0,
            Component = 1,
            BarVirtualization = 2,
            MemoryDevice = 3,
            Cpmu = 4
        ],
        /// Offset bits 31:16 (64 KiB aligned).
        OFFSET OFFSET(16) NUMBITS(16) []
    ]
];

/// Block identifiers of the Register Locator (CXL 3.1 Table 8-22).
pub mod block {
    pub const EMPTY: u8 = 0;
    pub const COMPONENT: u8 = 1;
    pub const BAR_VIRTUALIZATION: u8 = 2;
    pub const MEMORY_DEVICE: u8 = 3;
    pub const CPMU: u8 = 4;
}

/// One register block of a Register Locator: block `id` at `offset` into
/// the memory BAR `bir`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegisterBlock {
    pub bir: u8,
    pub id: u8,
    pub offset: u64,
}

impl RegisterBlock {
    /// Decode one Register Locator entry.
    pub fn decode(low: u32, high: u32) -> Self {
        let low = LocalRegisterCopy::<u32, RegisterOffsetLow::Register>::new(low);
        Self {
            bir: low.read(RegisterOffsetLow::BIR) as u8,
            id: low.read(RegisterOffsetLow::BLOCK_ID) as u8,
            offset: u64::from(high) << 32 | u64::from(low.read(RegisterOffsetLow::OFFSET)) << 16,
        }
    }

    /// The config offset of the BAR the block is in (`0x10 + 4 * bir`).
    pub const fn bar(&self) -> u16 {
        crate::config::PCI_BAR0 + 4 * self.bir as u16
    }

    /// The block's physical address, from the BAR PCI enumeration assigned.
    pub fn address(&self, device: &EcamDevice) -> Option<u64> {
        if self.bir > 5 {
            return None;
        }
        device.memory_bar(self.bar())?.checked_add(self.offset)
    }
}

/// The register blocks of `device`'s Register Locator, in entry order.
pub fn register_blocks(device: &EcamDevice) -> impl Iterator<Item = RegisterBlock> + '_ {
    let locator = super::find_dvsec(device, id::REGISTER_LOCATOR);
    let count = locator.map_or(0, |dvsec| {
        let bytes = (device.read32(dvsec + HEADER2) >> 20) as u16;
        bytes.saturating_sub(REGISTER_BLOCKS) / 8
    });
    (0..count).filter_map(move |i| {
        let entry = locator? + REGISTER_BLOCKS + 8 * i;
        let block = RegisterBlock::decode(device.read32(entry), device.read32(entry + 4));
        (block.id != block::EMPTY).then_some(block)
    })
}

/// The first register block `id` of `device`.
pub fn find_register_block(device: &EcamDevice, id: u8) -> Option<RegisterBlock> {
    register_blocks(device).find(|block| block.id == id)
}

/// One of a CXL device's (up to two) HDM ranges as its DVSEC reports it.
#[derive(Clone, Copy)]
pub struct Range {
    pub size: u64,
    pub base: u64,
    pub info: LocalRegisterCopy<u32, RangeSizeLow::Register>,
}

impl Range {
    /// Range `n` (`0..2`) of the device DVSEC at `dvsec`.
    pub fn read(device: &EcamDevice, dvsec: u16, n: u16) -> Self {
        let size_low = device.read32(dvsec + range_size_low(n));
        let base_low = device.read32(dvsec + range_base_low(n));
        let info = LocalRegisterCopy::<u32, RangeSizeLow::Register>::new(size_low);
        Self {
            size: u64::from(device.read32(dvsec + range_size_high(n))) << 32
                | u64::from(info.read(RangeSizeLow::SIZE_LOW)) << 28,
            base: u64::from(device.read32(dvsec + range_base_high(n))) << 32
                | u64::from(RangeBaseLow::BASE_LOW.read(base_low)) << 28,
            info,
        }
    }

    /// The device reported the range's size, and the media is usable.
    pub fn active(&self) -> bool {
        self.info.is_set(RangeSizeLow::MEMORY_INFO_VALID)
            && self.info.is_set(RangeSizeLow::MEMORY_ACTIVE)
    }
}

/// The CXL Lock register's bit (+0x14): the control and range registers
/// become read-only until reset.
pub const LOCK: u16 = 1 << 0;

/// The memory a device range holds (Range Size Low Media_Type).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaType {
    Volatile,
    NonVolatile,
    Other(u8),
}

impl Range {
    /// The range's Media_Type (`001`: non-volatile; CXL 3.1 Table 8-8).
    pub fn media_type(&self) -> MediaType {
        match self.info.read(RangeSizeLow::MEDIA_TYPE) as u8 {
            0 => MediaType::Volatile,
            1 => MediaType::NonVolatile,
            other => MediaType::Other(other),
        }
    }

    /// The device's desired interleave (Range Size Low bits 12:8).
    pub fn desired_interleave(&self) -> u8 {
        self.info.read(RangeSizeLow::DESIRED_INTERLEAVE) as u8
    }

    /// Program range `n`'s base (256 MiB granular; the low bits are not
    /// written) of the device DVSEC at `dvsec`.
    pub fn set_base(device: &EcamDevice, dvsec: u16, n: u16, base: u64) {
        device.write32(dvsec + range_base_high(n), (base >> 32) as u32);
        let mut low = LocalRegisterCopy::<u32, RangeBaseLow::Register>::new(0);
        low.write(RangeBaseLow::BASE_LOW.val(((base as u32) >> 28) & 0xf));
        device.write32(dvsec + range_base_low(n), low.get());
    }
}

/// The device DVSEC's CXL Capability (+0x0A).
pub fn capability(
    device: &EcamDevice,
    dvsec: u16,
) -> LocalRegisterCopy<u16, DeviceCapability::Register> {
    LocalRegisterCopy::new(device.read16(dvsec + DEVICE_CAPABILITY))
}

/// Turn on the device's memory decode (`Mem_Enable`) and, when it is
/// cache-capable and `cache` is set, `Cache_Enable` (the vendors'
/// `Control |= 4`, `|= 1` if Capability bit 0), keeping the other bits.
pub fn enable(device: &EcamDevice, dvsec: u16, cache: bool) {
    let cap = capability(device, dvsec);
    let mut control = LocalRegisterCopy::<u16, DeviceControl::Register>::new(
        device.read16(dvsec + DEVICE_CONTROL),
    );
    control.modify(DeviceControl::MEM_ENABLE::SET);
    if cache && cap.is_set(DeviceCapability::CACHE_CAPABLE) {
        control.modify(DeviceControl::CACHE_ENABLE::SET);
    }
    device.write16(dvsec + DEVICE_CONTROL, control.get());
}

/// Set CXL Lock (+0x14 bit 0).
pub fn lock(device: &EcamDevice, dvsec: u16) {
    device.write16(
        dvsec + DEVICE_LOCK,
        device.read16(dvsec + DEVICE_LOCK) | LOCK,
    );
}

/// CXL Extensions DVSEC for Ports (ID 3, CXL 3.1 §8.1.5): the alternate
/// ranges a switch or root port forwards to CXL.mem when it decodes
/// without HDM decoders ("Alt Memory and ID Space").
pub mod port_extensions {
    use tock_registers::register_bitfields;

    /// Port Control Extensions (+0x0C, 16-bit), [`PortControl`].
    pub const PORT_CONTROL: u16 = 0x0c;
    /// Alternate Memory Base / Limit (+0x10/+0x12, 16-bit: address bits
    /// 31:20 in bits 15:4).
    pub const ALT_MEMORY_BASE: u16 = 0x10;
    pub const ALT_MEMORY_LIMIT: u16 = 0x12;
    /// Alternate Prefetchable Memory Base / Limit (+0x14/+0x16, 16-bit)
    /// and their upper dwords (+0x18/+0x1C).
    pub const ALT_PREFETCH_BASE: u16 = 0x14;
    pub const ALT_PREFETCH_LIMIT: u16 = 0x16;
    pub const ALT_PREFETCH_BASE_HIGH: u16 = 0x18;
    pub const ALT_PREFETCH_LIMIT_HIGH: u16 = 0x1c;

    register_bitfields![u16,
        /// +0x0C (16-bit).
        pub PortControl [
            UNMASK_SBR OFFSET(0) NUMBITS(1) [],
            UNMASK_LINK_DISABLE OFFSET(1) NUMBITS(1) [],
            ALT_MEMORY_AND_ID_SPACE_ENABLE OFFSET(2) NUMBITS(1) [],
            ALT_BME OFFSET(3) NUMBITS(1) [],
            VIRAL_ENABLE OFFSET(14) NUMBITS(1) []
        ]
    ];

    /// Forward `base..base + size` (1 MiB granular) as the alternate
    /// prefetchable range of the port DVSEC at `dvsec`, and enable it.
    pub fn set_range(device: &crate::EcamDevice, dvsec: u16, base: u64, size: u64) {
        let limit = base + size.max(1 << 20) - 1;
        let low = |a: u64| ((a >> 16) as u16) & 0xfff0;
        device.write16(dvsec + ALT_PREFETCH_BASE, low(base) | 1);
        device.write16(dvsec + ALT_PREFETCH_LIMIT, low(limit) | 1);
        device.write32(dvsec + ALT_PREFETCH_BASE_HIGH, (base >> 32) as u32);
        device.write32(dvsec + ALT_PREFETCH_LIMIT_HIGH, (limit >> 32) as u32);
        let control = device.read16(dvsec + PORT_CONTROL);
        device.write16(
            dvsec + PORT_CONTROL,
            control | PortControl::ALT_MEMORY_AND_ID_SPACE_ENABLE::SET.value,
        );
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use crate::PciAddress;

    #[test]
    fn register_locator_blocks_and_device_ranges_decode() {
        let mut ram = [0u32; 1024];
        let device =
            EcamDevice::at_config_base(PciAddress::new(0, 1, 0, 0), ram.as_mut_ptr() as usize);
        device.write32(0, 0x0d93_1e98);
        // BAR2: 64-bit memory at 0x80_0000_0000.
        device.write32(0x18, 0x0000_000c);
        device.write32(0x1c, 0x80);
        // Device DVSEC at 0x100, Register Locator at 0x140.
        device.write32(0x100, 0x1401_0023);
        device.write32(0x104, 0x03c0_1e98);
        device.write16(0x108, id::DEVICE);
        device.write32(0x100 + range_size_high(0), 0x1);
        device.write32(0x100 + range_size_low(0), 0x3 | 0xa000_0000);
        device.write32(0x140, 0x0001_0023);
        device.write32(0x144, 0x01c0_1e98);
        device.write16(0x148, id::REGISTER_LOCATOR);
        // Component block in BAR2 at 0x1_0000, memdev block at 0x2_0000.
        device.write32(0x14c, 0x0001_0102);
        device.write32(0x150, 0);
        device.write32(0x154, 0x0002_0302);
        device.write32(0x158, 0);

        assert_eq!(super::super::find_dvsec(&device, id::DEVICE), Some(0x100));
        let range = Range::read(&device, 0x100, 0);
        assert_eq!(range.size, 0x1_a000_0000);
        assert!(range.active());

        let blocks: std::vec::Vec<_> = register_blocks(&device).collect();
        assert_eq!(blocks.len(), 2);
        let component = find_register_block(&device, block::COMPONENT).unwrap();
        assert_eq!((component.bir, component.offset), (2, 0x1_0000));
        assert_eq!(component.address(&device), Some(0x80_0001_0000));
        let memdev = find_register_block(&device, block::MEMORY_DEVICE).unwrap();
        assert_eq!(memdev.offset, 0x2_0000);
        assert_eq!(find_register_block(&device, block::CPMU), None);
        assert_eq!(range.media_type(), MediaType::Volatile);

        // Enable and lock: Mem_Enable, Cache_Enable only when capable.
        device.write16(0x100 + DEVICE_CAPABILITY, 0b100);
        Range::set_base(&device, 0x100, 0, 0x40_1000_0000 | 0xfff);
        assert_eq!(device.read32(0x100 + range_base_high(0)), 0x40);
        assert_eq!(device.read32(0x100 + range_base_low(0)), 0x1000_0000);
        enable(&device, 0x100, true);
        assert_eq!(device.read16(0x100 + DEVICE_CONTROL), 0b100);
        lock(&device, 0x100);
        assert_eq!(device.read16(0x100 + DEVICE_LOCK), 1);
        port_extensions::set_range(&device, 0x200, 0x40_0000_0000, 1 << 30);
        assert_eq!(device.read16(0x200 + port_extensions::ALT_PREFETCH_BASE), 1);
        assert_eq!(
            device.read16(0x200 + port_extensions::ALT_PREFETCH_LIMIT),
            0x3ff1
        );
        assert_eq!(
            device.read32(0x200 + port_extensions::ALT_PREFETCH_LIMIT_HIGH),
            0x40
        );
        assert_eq!(device.read16(0x200 + port_extensions::PORT_CONTROL), 4);
    }
}
