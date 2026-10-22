//! IVRS (I/O Virtualization Reporting Structure) for AMD IOMMUs.
//!
//! AMD I/O Virtualization Technology (IOMMU) Specification 48882. The IVHD
//! fields come from the IOMMU's own registers; the platform supplies the
//! register values and the device entries of every IOMMU.
//!
//! Every IOMMU gets an IVHD type 10h and, when it has an extended feature
//! register, a type 11h with the same device entries. Type 40h (and with it
//! the ACPI HID device entries) is optional: Linux only uses the IVHDs of the
//! highest type the first IOMMU has, so 40h is all or nothing.

use acpi_tables::sdt::Sdt;
use alloc::vec::Vec;
use tock_registers::{LocalRegisterCopy, fields::FieldValue, register_bitfields};
use zerocopy::{
    Immutable, IntoBytes,
    little_endian::{U16, U32, U64},
};

register_bitfields![u8,
    /// Device table entry settings of a device entry.
    pub Dte [
        INIT_PASS OFFSET(0) NUMBITS(1) [],
        EXT_INT_PASS OFFSET(1) NUMBITS(1) [],
        NMI_PASS OFFSET(2) NUMBITS(1) [],
        SYS_MGT OFFSET(4) NUMBITS(2) [
            TargetAbort = 0,
            NoTranslate = 1,
            IntxNoTranslate = 2,
            Translate = 3
        ],
        LINT0_PASS OFFSET(6) NUMBITS(1) [],
        LINT1_PASS OFFSET(7) NUMBITS(1) []
    ],
    /// IVHD flags; bits 7:6 are reserved in types 11h and 40h.
    Flags [
        HT_TUN_EN OFFSET(0) NUMBITS(1) [],
        PASS_PW OFFSET(1) NUMBITS(1) [],
        RES_PASS_PW OFFSET(2) NUMBITS(1) [],
        ISOC OFFSET(3) NUMBITS(1) [],
        IOTLB_SUP OFFSET(4) NUMBITS(1) [],
        COHERENT OFFSET(5) NUMBITS(1) [],
        PREF_SUP OFFSET(6) NUMBITS(1) [],
        PPR_SUP OFFSET(7) NUMBITS(1) []
    ]
];
register_bitfields![u16,
    Info [
        MSI_NUM OFFSET(0) NUMBITS(5) [],
        UNIT_ID OFFSET(8) NUMBITS(5) []
    ]
];
register_bitfields![u32,
    /// IOMMU capability header (capability + 0x00).
    Capability [
        IOTLB_SUP OFFSET(24) NUMBITS(1) [],
        EFR_SUP OFFSET(27) NUMBITS(1) []
    ],
    /// IOMMU range register (capability + 0x0c).
    Range [UNIT_ID OFFSET(0) NUMBITS(5) []],
    /// IOMMU miscellaneous information register 0 (capability + 0x10).
    Misc [
        MSI_NUM OFFSET(0) NUMBITS(5) [],
        GVA_SIZE OFFSET(5) NUMBITS(3) [],
        PA_SIZE OFFSET(8) NUMBITS(7) [],
        VA_SIZE OFFSET(15) NUMBITS(7) [],
        HT_ATS_RESV OFFSET(22) NUMBITS(1) [],
        MSI_NUM_PPR OFFSET(27) NUMBITS(5) []
    ],
    IvInfo [
        EFR_SUP OFFSET(0) NUMBITS(1) [],
        GVA_SIZE OFFSET(5) NUMBITS(3) [],
        PA_SIZE OFFSET(8) NUMBITS(7) [],
        VA_SIZE OFFSET(15) NUMBITS(7) [],
        HT_ATS_RESV OFFSET(22) NUMBITS(1) []
    ],
    /// IVHD 10h IOMMU feature reporting. Only MSI_NUM_PPR, PN_BANKS and
    /// PN_COUNTERS exist in the 11h/40h IOMMU attributes.
    Feature [
        XT_SUP OFFSET(0) NUMBITS(1) [],
        NX_SUP OFFSET(1) NUMBITS(1) [],
        GT_SUP OFFSET(2) NUMBITS(1) [],
        GLX OFFSET(3) NUMBITS(2) [],
        IA_SUP OFFSET(5) NUMBITS(1) [],
        GA_SUP OFFSET(6) NUMBITS(1) [],
        HE_SUP OFFSET(7) NUMBITS(1) [],
        PAS_MAX OFFSET(8) NUMBITS(5) [],
        PN_COUNTERS OFFSET(13) NUMBITS(4) [],
        PN_BANKS OFFSET(17) NUMBITS(6) [],
        MSI_NUM_PPR OFFSET(23) NUMBITS(5) [],
        GATS OFFSET(28) NUMBITS(2) [],
        HATS OFFSET(30) NUMBITS(2) []
    ]
];
register_bitfields![u64,
    /// MMIO 0x18
    Control [
        HT_TUN_EN OFFSET(1) NUMBITS(1) [],
        PASS_PW OFFSET(8) NUMBITS(1) [],
        RES_PASS_PW OFFSET(9) NUMBITS(1) [],
        COHERENT OFFSET(10) NUMBITS(1) [],
        ISOC OFFSET(11) NUMBITS(1) []
    ],
    /// MMIO 0x30: extended feature register
    Efr [
        PREF_SUP OFFSET(0) NUMBITS(1) [],
        PPR_SUP OFFSET(1) NUMBITS(1) [],
        XT_SUP OFFSET(2) NUMBITS(1) [],
        NX_SUP OFFSET(3) NUMBITS(1) [],
        GT_SUP OFFSET(4) NUMBITS(1) [],
        IA_SUP OFFSET(6) NUMBITS(1) [],
        GA_SUP OFFSET(7) NUMBITS(1) [],
        HE_SUP OFFSET(8) NUMBITS(1) [],
        HATS OFFSET(10) NUMBITS(2) [],
        GATS OFFSET(12) NUMBITS(2) [],
        GLX OFFSET(14) NUMBITS(2) [],
        PAS_MAX OFFSET(32) NUMBITS(5) []
    ],
    /// MMIO 0x4000: counter configuration
    CounterConfig [
        N_COUNTERS OFFSET(7) NUMBITS(4) [],
        N_BANKS OFFSET(12) NUMBITS(6) []
    ]
];

/// The IOMMU registers the IVHD fields are derived from.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IommuRegisters {
    /// Capability header, range and miscellaneous information 0 registers
    /// (capability + 0x00, + 0x0c, + 0x10).
    pub capability: u32,
    pub range: u32,
    pub misc: u32,
    /// MMIO 0x18, 0x30, 0x1a0 and 0x4000: control, EFR, EFR2 and counter
    /// configuration.
    pub control: u64,
    pub efr: u64,
    pub efr2: u64,
    pub counter_config: u64,
}

/// A special device whose requests carry another device's ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Variety {
    IoApic = 1,
    Hpet = 2,
}

/// An IVHD device entry. Device IDs are `bus << 8 | device << 3 | function`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceEntry {
    Select {
        device: u16,
        dte: u8,
    },
    /// All IDs in `start..=end`.
    Range {
        start: u16,
        end: u16,
        dte: u8,
    },
    /// The IDs `start..=end` appear as `alias` (behind a PCI bridge).
    AliasRange {
        start: u16,
        end: u16,
        alias: u16,
        dte: u8,
    },
    /// The IOAPIC with ID `handle` (or HPET number `handle`) sends its
    /// interrupts with the ID `source`.
    Special {
        variety: Variety,
        handle: u8,
        source: u16,
        dte: u8,
    },
    /// An ACPI device with an 8-character `_HID`, IVHD 40h only.
    AcpiHid {
        device: u16,
        hid: [u8; 8],
        dte: u8,
    },
}

/// One IOMMU (an IVHD block).
#[derive(Debug, Clone, Copy)]
pub struct Iommu<'a> {
    /// The IOMMU's own PCI ID and the offset of its capability.
    pub device_id: u16,
    pub capability_offset: u16,
    pub segment: u16,
    pub base: u64,
    pub registers: IommuRegisters,
    pub devices: &'a [DeviceEntry],
}

pub struct IvrsConfig<'a> {
    pub iommus: &'a [Iommu<'a>],
    /// Add IVHD 40h blocks with the ACPI HID entries.
    pub acpi_hid: bool,
}

#[derive(IntoBytes, Immutable)]
#[repr(C)]
struct IvhdHeader {
    kind: u8,
    flags: u8,
    length: U16,
    device_id: U16,
    capability_offset: U16,
    base: U64,
    segment: U16,
    info: U16,
    /// IOMMU feature reporting (10h) or attributes (11h, 40h).
    feature: U32,
}

/// The rest of an IVHD 11h/40h header.
#[derive(IntoBytes, Immutable)]
#[repr(C)]
struct IvhdEfr {
    efr: U64,
    efr2: U64,
}

#[derive(IntoBytes, Immutable)]
#[repr(C)]
struct Entry {
    kind: u8,
    device: U16,
    dte: u8,
}

#[derive(IntoBytes, Immutable)]
#[repr(C)]
struct AliasEntry {
    kind: u8,
    device: U16,
    dte: u8,
    reserved: u8,
    source: U16,
    reserved2: u8,
}

#[derive(IntoBytes, Immutable)]
#[repr(C)]
struct SpecialEntry {
    kind: u8,
    reserved: U16,
    dte: u8,
    handle: u8,
    source: U16,
    variety: u8,
}

#[derive(IntoBytes, Immutable)]
#[repr(C)]
struct HidEntry {
    kind: u8,
    device: U16,
    dte: u8,
    hid: [u8; 8],
    cid: [u8; 8],
    uid_format: u8,
    uid_length: u8,
}

const IVHD_10: u8 = 0x10;
const IVHD_11: u8 = 0x11;
const IVHD_40: u8 = 0x40;

const SELECT: u8 = 0x02;
const RANGE_START: u8 = 0x03;
const RANGE_END: u8 = 0x04;
const ALIAS_RANGE_START: u8 = 0x43;
const SPECIAL: u8 = 0x48;
const ACPI_HID: u8 = 0xf0;

/// Build the IVRS, `None` without an IOMMU. IVinfo comes from the first one.
pub fn build_ivrs(config: &IvrsConfig) -> Option<Vec<u8>> {
    let first = config.iommus.first()?;
    let body: Vec<u8> = iv_info(&first.registers)
        .to_le_bytes()
        .into_iter()
        .chain([0; 8])
        .chain(
            config
                .iommus
                .iter()
                .flat_map(|iommu| blocks(iommu, config.acpi_hid)),
        )
        .collect();
    let mut sdt = Sdt::new(
        *b"IVRS",
        36,
        2, // types 10h, 11h and 40h
        crate::OEM_ID,
        crate::OEM_TABLE_ID,
        crate::OEM_REVISION,
    );
    sdt.append_slice(&body);
    Some(crate::serialize(&sdt))
}

fn iv_info(registers: &IommuRegisters) -> u32 {
    let capability = LocalRegisterCopy::<u32, Capability::Register>::new(registers.capability);
    let misc = LocalRegisterCopy::<u32, Misc::Register>::new(registers.misc);
    (IvInfo::EFR_SUP.val(capability.read(Capability::EFR_SUP))
        + IvInfo::GVA_SIZE.val(misc.read(Misc::GVA_SIZE))
        + IvInfo::PA_SIZE.val(misc.read(Misc::PA_SIZE))
        + IvInfo::VA_SIZE.val(misc.read(Misc::VA_SIZE))
        + IvInfo::HT_ATS_RESV.val(misc.read(Misc::HT_ATS_RESV)))
    .value
}

/// 10h, then 11h and 40h if the IOMMU has an EFR.
fn blocks(iommu: &Iommu, acpi_hid: bool) -> Vec<u8> {
    let efr = LocalRegisterCopy::<u32, Capability::Register>::new(iommu.registers.capability)
        .is_set(Capability::EFR_SUP);
    let kinds: &[u8] = match (efr, acpi_hid) {
        (false, _) => &[IVHD_10],
        (true, false) => &[IVHD_10, IVHD_11],
        (true, true) => &[IVHD_10, IVHD_11, IVHD_40],
    };
    kinds.iter().flat_map(|&kind| ivhd(kind, iommu)).collect()
}

fn ivhd(kind: u8, iommu: &Iommu) -> Vec<u8> {
    let r = &iommu.registers;
    let entries: Vec<u8> = iommu
        .devices
        .iter()
        .filter(|entry| kind == IVHD_40 || !matches!(entry, DeviceEntry::AcpiHid { .. }))
        .flat_map(device_entry)
        .collect();
    let efr = (kind != IVHD_10).then(|| IvhdEfr {
        efr: r.efr.into(),
        efr2: r.efr2.into(),
    });
    let header_length =
        core::mem::size_of::<IvhdHeader>() + efr.as_ref().map_or(0, |efr| efr.as_bytes().len());
    let (flags, feature) = if kind == IVHD_10 {
        (flags(r).value, feature(r))
    } else {
        let reserved = (Flags::PREF_SUP::SET + Flags::PPR_SUP::SET).value;
        (flags(r).value & !reserved, attributes(r).value)
    };
    let header = IvhdHeader {
        kind,
        flags,
        length: ((header_length + entries.len()) as u16).into(),
        device_id: iommu.device_id.into(),
        capability_offset: iommu.capability_offset.into(),
        base: iommu.base.into(),
        segment: iommu.segment.into(),
        info: info(r).into(),
        feature: feature.into(),
    };
    header
        .as_bytes()
        .iter()
        .chain(efr.as_ref().map_or(&[][..], |efr| efr.as_bytes()))
        .chain(&entries)
        .copied()
        .collect()
}

fn flags(r: &IommuRegisters) -> FieldValue<u8, Flags::Register> {
    let capability = LocalRegisterCopy::<u32, Capability::Register>::new(r.capability);
    let control = LocalRegisterCopy::<u64, Control::Register>::new(r.control);
    let efr = LocalRegisterCopy::<u64, Efr::Register>::new(r.efr);
    Flags::HT_TUN_EN.val(control.read(Control::HT_TUN_EN) as u8)
        + Flags::PASS_PW.val(control.read(Control::PASS_PW) as u8)
        + Flags::RES_PASS_PW.val(control.read(Control::RES_PASS_PW) as u8)
        + Flags::ISOC.val(control.read(Control::ISOC) as u8)
        + Flags::IOTLB_SUP.val(capability.read(Capability::IOTLB_SUP) as u8)
        + Flags::COHERENT.val(control.read(Control::COHERENT) as u8)
        + Flags::PREF_SUP.val(efr.read(Efr::PREF_SUP) as u8)
        + Flags::PPR_SUP.val(efr.read(Efr::PPR_SUP) as u8)
}

fn info(r: &IommuRegisters) -> u16 {
    let misc = LocalRegisterCopy::<u32, Misc::Register>::new(r.misc);
    let range = LocalRegisterCopy::<u32, Range::Register>::new(r.range);
    (Info::MSI_NUM.val(misc.read(Misc::MSI_NUM) as u16)
        + Info::UNIT_ID.val(range.read(Range::UNIT_ID) as u16))
    .value
}

/// The IVHD 11h/40h IOMMU attributes, a subset of the 10h feature field.
fn attributes(r: &IommuRegisters) -> FieldValue<u32, Feature::Register> {
    let misc = LocalRegisterCopy::<u32, Misc::Register>::new(r.misc);
    let counters = LocalRegisterCopy::<u64, CounterConfig::Register>::new(r.counter_config);
    Feature::MSI_NUM_PPR.val(misc.read(Misc::MSI_NUM_PPR))
        + Feature::PN_BANKS.val(counters.read(CounterConfig::N_BANKS) as u32)
        + Feature::PN_COUNTERS.val(counters.read(CounterConfig::N_COUNTERS) as u32)
}

fn feature(r: &IommuRegisters) -> u32 {
    let efr = LocalRegisterCopy::<u64, Efr::Register>::new(r.efr);
    let from = |field: tock_registers::fields::Field<u64, Efr::Register>| efr.read(field) as u32;
    (attributes(r)
        + Feature::HATS.val(from(Efr::HATS))
        + Feature::GATS.val(from(Efr::GATS))
        + Feature::PAS_MAX.val(from(Efr::PAS_MAX))
        + Feature::HE_SUP.val(from(Efr::HE_SUP))
        + Feature::GA_SUP.val(from(Efr::GA_SUP))
        + Feature::IA_SUP.val(from(Efr::IA_SUP))
        + Feature::GLX.val(from(Efr::GLX))
        + Feature::GT_SUP.val(from(Efr::GT_SUP))
        + Feature::NX_SUP.val(from(Efr::NX_SUP))
        + Feature::XT_SUP.val(from(Efr::XT_SUP)))
    .value
}

fn device_entry(entry: &DeviceEntry) -> Vec<u8> {
    let entry4 = |kind, device: u16, dte| Entry {
        kind,
        device: device.into(),
        dte,
    };
    match *entry {
        DeviceEntry::Select { device, dte } => entry4(SELECT, device, dte).as_bytes().to_vec(),
        DeviceEntry::Range { start, end, dte } => [
            entry4(RANGE_START, start, dte).as_bytes(),
            entry4(RANGE_END, end, 0).as_bytes(),
        ]
        .concat(),
        DeviceEntry::AliasRange {
            start,
            end,
            alias,
            dte,
        } => [
            AliasEntry {
                kind: ALIAS_RANGE_START,
                device: start.into(),
                dte,
                reserved: 0,
                source: alias.into(),
                reserved2: 0,
            }
            .as_bytes(),
            entry4(RANGE_END, end, 0).as_bytes(),
        ]
        .concat(),
        DeviceEntry::Special {
            variety,
            handle,
            source,
            dte,
        } => SpecialEntry {
            kind: SPECIAL,
            reserved: U16::ZERO,
            dte,
            handle,
            source: source.into(),
            variety: variety as u8,
        }
        .as_bytes()
        .to_vec(),
        DeviceEntry::AcpiHid { device, hid, dte } => HidEntry {
            kind: ACPI_HID,
            device: device.into(),
            dte,
            hid,
            cid: [0; 8],
            uid_format: 0,
            uid_length: 0,
        }
        .as_bytes()
        .to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Zeppelin-like IOMMU: EFR, IOTLB, 40-bit PA, 64-bit VA.
    const REGISTERS: IommuRegisters = IommuRegisters {
        capability: 0x0003_000f | 1 << 24 | 1 << 27,
        range: 0x0000_0002,
        misc: 0x1 << 27 | 0x40 << 15 | 0x30 << 8 | 0x2 << 5 | 0x3,
        control: 1 << 10,
        efr: 0x2_0000_0000 | 3 << 10 | 1 << 14 | 1 << 1 | 1 << 2,
        efr2: 0x10,
        counter_config: 2 << 12 | 4 << 7,
    };
    const DEVICES: [DeviceEntry; 4] = [
        DeviceEntry::Range {
            start: 0x0003,
            end: 0x7fff,
            dte: 0,
        },
        DeviceEntry::Special {
            variety: Variety::Hpet,
            handle: 0,
            source: 0x00a0,
            dte: 0,
        },
        DeviceEntry::Special {
            variety: Variety::IoApic,
            handle: 0,
            source: 0x00a0,
            dte: 0xd7,
        },
        DeviceEntry::AcpiHid {
            device: 0x0099,
            hid: *b"AMDI0040",
            dte: 0xf7,
        },
    ];

    fn iommu(devices: &[DeviceEntry]) -> Iommu<'_> {
        Iommu {
            device_id: 0x0002,
            capability_offset: 0x40,
            segment: 0,
            base: 0xfe60_0000,
            registers: REGISTERS,
            devices,
        }
    }

    fn u16_at(bytes: &[u8], at: usize) -> u16 {
        u16::from_le_bytes(bytes[at..at + 2].try_into().unwrap())
    }
    fn u32_at(bytes: &[u8], at: usize) -> u32 {
        u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
    }

    /// (type, offset) of the IVHD blocks.
    fn blocks_of(ivrs: &[u8]) -> std::vec::Vec<(u8, usize)> {
        let mut at = 48;
        let mut blocks = std::vec::Vec::new();
        while at < ivrs.len() {
            blocks.push((ivrs[at], at));
            at += usize::from(u16_at(ivrs, at + 2));
        }
        assert_eq!(at, ivrs.len());
        blocks
    }

    #[test]
    fn dte_matches_the_fch_ioapic_setting() {
        let dte = Dte::LINT1_PASS::SET
            + Dte::LINT0_PASS::SET
            + Dte::SYS_MGT::NoTranslate
            + Dte::NMI_PASS::SET
            + Dte::EXT_INT_PASS::SET
            + Dte::INIT_PASS::SET;
        assert_eq!(dte.value, 0xd7);
    }

    #[test]
    fn header_and_ivhd_10h_fields_come_from_the_registers() {
        let iommus = [iommu(&DEVICES[..3])];
        let ivrs = build_ivrs(&IvrsConfig {
            iommus: &iommus,
            acpi_hid: false,
        })
        .unwrap();
        assert_eq!(&ivrs[..4], b"IVRS");
        assert_eq!(u32_at(&ivrs, 4) as usize, ivrs.len());
        assert_eq!(ivrs.iter().fold(0u8, |sum, b| sum.wrapping_add(*b)), 0);
        assert_eq!(u32_at(&ivrs, 36), 0x40 << 15 | 0x30 << 8 | 0x2 << 5 | 1);

        assert_eq!(blocks_of(&ivrs), [(0x10, 48), (0x11, 48 + 24 + 24)]);
        let ivhd = &ivrs[48..];
        // PPR, coherent, IOTLB
        assert_eq!(ivhd[1], 0x80 | 0x20 | 0x10);
        assert_eq!(u16_at(ivhd, 2), 24 + 8 + 8 + 8);
        assert_eq!(u16_at(ivhd, 4), 0x0002);
        assert_eq!(u16_at(ivhd, 6), 0x40);
        assert_eq!(&ivhd[8..16], &0xfe60_0000u64.to_le_bytes());
        assert_eq!(u16_at(ivhd, 18), 0x0203);
        assert_eq!(
            u32_at(ivhd, 20),
            3 << 30 | 1 << 23 | 2 << 17 | 4 << 13 | 2 << 8 | 1 << 3 | 1
        );
        assert_eq!(
            &ivhd[24..48],
            [
                0x03, 0x03, 0x00, 0x00, 0x04, 0xff, 0x7f, 0x00, // range
                0x48, 0, 0, 0x00, 0x00, 0xa0, 0x00, 0x02, // HPET 0
                0x48, 0, 0, 0xd7, 0x00, 0xa0, 0x00, 0x01, // IOAPIC 0
            ]
        );
    }

    #[test]
    fn ivhd_11h_has_the_attributes_and_efr_images() {
        let iommus = [iommu(&DEVICES[..1])];
        let ivrs = build_ivrs(&IvrsConfig {
            iommus: &iommus,
            acpi_hid: false,
        })
        .unwrap();
        let ivhd = &ivrs[48 + 24 + 8..];
        assert_eq!(ivhd[0], 0x11);
        assert_eq!(ivhd[1], 0x20 | 0x10);
        assert_eq!(u16_at(ivhd, 2), 40 + 8);
        assert_eq!(u32_at(ivhd, 20), 1 << 23 | 2 << 17 | 4 << 13);
        assert_eq!(&ivhd[24..32], &REGISTERS.efr.to_le_bytes());
        assert_eq!(&ivhd[32..40], &REGISTERS.efr2.to_le_bytes());
    }

    #[test]
    fn acpi_hid_entries_only_in_ivhd_40h_and_no_11h_without_efr() {
        let iommus = [iommu(&DEVICES)];
        let ivrs = build_ivrs(&IvrsConfig {
            iommus: &iommus,
            acpi_hid: true,
        })
        .unwrap();
        let blocks = blocks_of(&ivrs);
        assert_eq!(
            blocks.iter().map(|b| b.0).collect::<std::vec::Vec<_>>(),
            [0x10, 0x11, 0x40]
        );
        let ivhd40 = &ivrs[blocks[2].1..];
        assert_eq!(u16_at(ivhd40, 2), 40 + 24 + 22);
        let hid = &ivhd40[40 + 24..];
        assert_eq!(&hid[..4], [0xf0, 0x99, 0x00, 0xf7]);
        assert_eq!(&hid[4..12], b"AMDI0040");
        assert_eq!(&hid[12..22], [0; 10]);

        let mut legacy = iommu(&DEVICES);
        legacy.registers.capability &= !(1 << 27);
        let ivrs = build_ivrs(&IvrsConfig {
            iommus: &[legacy, iommu(&DEVICES[..1])],
            acpi_hid: true,
        })
        .unwrap();
        assert_eq!(ivrs[36] & 1, 0);
        assert_eq!(
            blocks_of(&ivrs)
                .iter()
                .map(|b| b.0)
                .collect::<std::vec::Vec<_>>(),
            [0x10, 0x10, 0x11, 0x40]
        );
        assert!(
            build_ivrs(&IvrsConfig {
                iommus: &[],
                acpi_hid: false
            })
            .is_none()
        );
    }

    #[test]
    fn alias_range_and_select_entries() {
        let devices = [
            DeviceEntry::AliasRange {
                start: 0x0100,
                end: 0x01ff,
                alias: 0x00a4,
                dte: 0,
            },
            DeviceEntry::Select {
                device: 0x0008,
                dte: 0x01,
            },
        ];
        let bytes: std::vec::Vec<u8> = devices.iter().flat_map(device_entry).collect();
        assert_eq!(
            bytes,
            [
                0x43, 0x00, 0x01, 0, 0, 0xa4, 0x00, 0, 0x04, 0xff, 0x01, 0, 0x02, 0x08, 0x00, 0x01
            ]
        );
    }
}
