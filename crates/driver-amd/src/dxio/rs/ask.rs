// SPDX-License-Identifier: MIT OR Apache-2.0
//! The ASK, MPIO's 52-byte description of one port, and the translation of
//! the board's lanes to an IOS's PCIe wrapper: the S8040's V2.00
//! AmdNbioPei (instruction-checked, trained on the S8040 and the Onyx);
//! openSIL's Genoa MPIO code names the fields.
//!
//! The lanes are translated to the present IOS by their physical index
//! (`ios`), which a fused-off IOS does not renumber.

use tock_registers::{LocalRegisterCopy, register_bitfields};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned, little_endian::U32};

register_bitfields![u32,
    pub Lane [
        START OFFSET(0) NUMBITS(16) [],
        COUNT OFFSET(16) NUMBITS(6) [],
        REVERSED OFFSET(22) NUMBITS(1) [],
        CONTROLLER OFFSET(28) NUMBITS(4) []
    ],
    pub Handle [
        GPIO OFFSET(0) NUMBITS(8) [],
        CHANNEL OFFSET(8) NUMBITS(8) [],
        ANCILLARY OFFSET(16) NUMBITS(16) []
    ],
    pub Device [
        FUNCTION OFFSET(0) NUMBITS(3) [],
        DEVICE OFFSET(3) NUMBITS(5) []
    ],
    pub Link [
        PRESENT OFFSET(0) NUMBITS(1) [],
        EARLY OFFSET(1) NUMBITS(1) [],
        COMPLIANCE OFFSET(2) NUMBITS(1) [],
        HOTPLUG OFFSET(4) NUMBITS(4) [],
        MAX_SPEED OFFSET(8) NUMBITS(4) [],
        TARGET_SPEED OFFSET(12) NUMBITS(4) [],
        /// WH: master PLL (`InitAskEntryAttributes`, Dell `0xf799b0`).
        MASTER_PLL OFFSET(26) NUMBITS(1) [],
        /// WH: invert receive polarity.
        RX_POLARITY OFFSET(27) NUMBITS(1) [],
        /// WH: invert transmit polarity.
        TX_POLARITY OFFSET(28) NUMBITS(1) []
    ],
    /// ASK `+0x10`, WH only (Dell `0xf799b0`).
    pub Phy [
        DEEMPHASIS OFFSET(16) NUMBITS(2) [],
        DEEMPHASIS_OVERRIDE OFFSET(18) NUMBITS(1) [],
        TX_VETTING OFFSET(19) NUMBITS(1) [],
        RX_VETTING OFFSET(20) NUMBITS(1) []
    ],
    /// ASK `+0x14`; all but the logical bridge WH only (Dell `0xf799b0`).
    pub Transport [
        /// UBM host-facing connector index.
        HFC OFFSET(0) NUMBITS(8) [],
        /// UBM drive-facing connector index.
        DFC OFFSET(8) NUMBITS(8) [],
        LOGICAL_BRIDGE OFFSET(16) NUMBITS(5) [],
        SWING OFFSET(21) NUMBITS(3) [],
        SRIS_SKIP_INTERVAL OFFSET(24) NUMBITS(3) [],
        /// Descriptor `+0x42[7:6]` (Dell; GIGABYTE bit 27 only), unnamed.
        EXTRA OFFSET(27) NUMBITS(2) []
    ],
    /// ASK `+0x18`, WH only.
    pub Attr [
        /// Early-trained or chipset link (`CsLink == 2`).
        EARLY OFFSET(1) NUMBITS(1) []
    ],
    // NFX's engine lookup compares ASK+0x22 with the native engine's
    // wrapper-local PortId. This is NOT the global devfn-remap slot/LBID.
    pub Status [
        STATE OFFSET(0) NUMBITS(4) [],
        LOCAL_PORT_ID OFFSET(16) NUMBITS(8) []
    ]
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum LinkSpeed {
    Auto = 0,
    Gen1,
    Gen2,
    Gen3,
    Gen4,
    Gen5,
    Gen6,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Pcie,
    Sata,
}

/// Native port policy, not AGESA's packed 0x21b-byte engine descriptor.
/// Fields absent here (ancillary PHY overrides, CXL, UBM) are not claimed
/// implemented. No serialization of unknown host padding is necessary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Port {
    pub start: u8,
    pub end: u8,
    pub kind: Kind,
    pub gpio_group: u8,
    pub present: bool,
    /// Requested device/function, not the trained result BDF; (0,0) = auto.
    pub device: u8,
    pub function: u8,
    pub max_speed: LinkSpeed,
    pub target_speed: LinkSpeed,
    pub compliance: bool,
    pub early_train: bool,
    pub hotplug: u8,
}
impl Port {
    pub const fn pcie(start: u8, end: u8) -> Self {
        Self {
            start,
            end,
            kind: Kind::Pcie,
            gpio_group: 1,
            present: true,
            device: 0,
            function: 0,
            max_speed: LinkSpeed::Auto,
            target_speed: LinkSpeed::Auto,
            compliance: false,
            early_train: false,
            hotplug: 0,
        }
    }
}

/// The actual retained-firmware entry: 13 little-endian dwords, no native
/// pointer, no implicit padding. Reserved/output words start zero.
#[derive(Clone, Copy, Debug, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
pub struct Ask {
    lane: U32,
    handle: U32,
    device: U32,
    link: U32,
    phy: U32,
    transport: U32,
    reserved0: [U32; 2],
    status: U32,
    reserved1: [U32; 4],
}
const _: () = assert!(size_of::<Ask>() == 0x34);

impl Ask {
    /// An ASK from its 13 dwords, as the host sends it.
    pub fn from_dwords(words: [u32; 13]) -> Self {
        Self {
            lane: words[0].into(),
            handle: words[1].into(),
            device: words[2].into(),
            link: words[3].into(),
            phy: words[4].into(),
            transport: words[5].into(),
            reserved0: [words[6].into(), words[7].into()],
            status: words[8].into(),
            reserved1: [
                words[9].into(),
                words[10].into(),
                words[11].into(),
                words[12].into(),
            ],
        }
    }
    /// The 13 dwords.
    pub fn dwords(&self) -> [u32; 13] {
        let r = self.reserved1.map(|w| w.get());
        [
            self.lane.get(),
            self.handle.get(),
            self.device.get(),
            self.link.get(),
            self.phy.get(),
            self.transport.get(),
            self.reserved0[0].get(),
            self.reserved0[1].get(),
            self.status.get(),
            r[0],
            r[1],
            r[2],
            r[3],
        ]
    }
    pub fn bytes(&self) -> &[u8] {
        self.as_bytes()
    }
    /// Only bits20:16 change when resending after training.
    pub fn set_logical_bridge(&mut self, id: u8) -> Result<(), Error> {
        if id >= 32 {
            return Err(Error::LogicalBridge(id));
        }
        self.transport.set(
            Transport::LOGICAL_BRIDGE
                .val(id.into())
                .modify(self.transport.get()),
        );
        Ok(())
    }
    pub fn training_state(&self) -> u32 {
        self.status.get()
    }
    pub fn lane(&self) -> LocalRegisterCopy<u32, Lane::Register> {
        LocalRegisterCopy::new(self.lane.get())
    }
    pub fn status(&self) -> LocalRegisterCopy<u32, Status::Register> {
        LocalRegisterCopy::new(self.status.get())
    }
    pub fn kind(&self) -> Option<Kind> {
        match Lane::CONTROLLER.read(self.lane.get()) {
            0 => Some(Kind::Pcie),
            1 => Some(Kind::Sata),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mapping {
    pub root_index: u8,
    pub wrapper: u8,
    pub start: u8,
    pub end: u8,
    pub reversed: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    RootIndex(u8),
    DuplicateRoot(u8),
    NoLaneWindow,
    CrossesWindow,
    Width(u8),
    DeviceFunction,
    Hotplug(u8),
    LogicalBridge(u8),
}

const START: [[u8; 2]; 4] = [[0, 96], [32, 64], [48, 112], [16, 80]];
const REVERSED: [[bool; 2]; 4] = [[false, true], [false, true], [true, false], [true, false]];

/// Unlike the donor, require BOTH endpoints inside the selected wrapper,
/// validate indices, and never silently drop an unrouteable port.
pub fn translate(roots: &[u8], start: u8, end: u8) -> Result<Mapping, Error> {
    let mut seen = 0u8;
    for &root in roots {
        if root >= 4 {
            return Err(Error::RootIndex(root));
        }
        if seen & (1 << root) != 0 {
            return Err(Error::DuplicateRoot(root));
        }
        seen |= 1 << root;
    }
    for &root in roots {
        for wrapper in 0..3u8 {
            let (base, width, reverse) = if wrapper < 2 {
                (
                    START[root as usize][wrapper as usize],
                    16,
                    REVERSED[root as usize][wrapper as usize],
                )
            } else if root.is_multiple_of(2) {
                (if root == 0 { 132 } else { 128 }, 4, false)
            } else {
                continue;
            };
            let last = base + width - 1;
            if !(base..=last).contains(&start) {
                continue;
            }
            if !(base..=last).contains(&end) {
                return Err(Error::CrossesWindow);
            }
            let mirror = |lane| if reverse { last - lane + base } else { lane };
            let (a, b) = (mirror(start), mirror(end));
            return Ok(Mapping {
                root_index: root,
                wrapper,
                start: a.min(b),
                end: a.max(b),
                reversed: reverse ^ (start > end),
            });
        }
    }
    Err(Error::NoLaneWindow)
}

/// Build one ASK entry. Ancillary indices are explicitly supplied by the
/// eventual ancillary-buffer owner; no virtual address is cast to firmware.
pub fn build(roots: &[u8], port: &Port, ancillary_index: u16) -> Result<(Mapping, Ask), Error> {
    let mapping = translate(roots, port.start, port.end)?;
    let ask = build_mapped(mapping, port, ancillary_index)?;
    Ok((mapping, ask))
}

/// Build one 52-byte ASK entry from an already-validated [`Mapping`] (used by
/// both RS wrapper translation and WH dual-IOD direct lane mapping).
pub fn build_mapped(mapping: Mapping, port: &Port, ancillary_index: u16) -> Result<Ask, Error> {
    if port.device > 31 || port.function > 7 {
        return Err(Error::DeviceFunction);
    }
    if port.hotplug > 7 {
        return Err(Error::Hotplug(port.hotplug));
    }
    let width = mapping.end - mapping.start + 1;
    if ![1, 2, 4, 8, 16].contains(&width) {
        return Err(Error::Width(width));
    }
    let mut lane = LocalRegisterCopy::<u32, Lane::Register>::new(0);
    lane.write(
        Lane::START.val(mapping.start.into())
            + Lane::COUNT.val(width.into())
            + Lane::REVERSED.val(mapping.reversed.into())
            + Lane::CONTROLLER.val(u32::from(port.kind == Kind::Sata)),
    );
    let mut handle = LocalRegisterCopy::<u32, Handle::Register>::new(0);
    handle.write(
        Handle::GPIO.val(port.gpio_group.into()) + Handle::ANCILLARY.val(ancillary_index.into()),
    );
    let mut device = LocalRegisterCopy::<u32, Device::Register>::new(0);
    device
        .write(Device::DEVICE.val(port.device.into()) + Device::FUNCTION.val(port.function.into()));
    let mut link = LocalRegisterCopy::<u32, Link::Register>::new(0);
    link.write(
        Link::PRESENT.val(port.present.into())
            + Link::EARLY.val(port.early_train.into())
            + Link::COMPLIANCE.val(port.compliance.into())
            + Link::HOTPLUG.val(port.hotplug.into())
            + Link::MAX_SPEED.val(port.max_speed as u32)
            + Link::TARGET_SPEED.val(if port.early_train {
                1
            } else {
                port.target_speed as u32
            }),
    );
    Ok(Ask {
        lane: lane.get().into(),
        handle: handle.get().into(),
        device: device.get().into(),
        link: link.get().into(),
        phy: 0.into(),
        transport: 0.into(),
        reserved0: [0.into(); 2],
        status: 0.into(),
        reserved1: [0.into(); 4],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lane_windows_are_explicit_not_assumed_from_root_count() {
        assert_eq!(
            translate(&[3], 16, 31).unwrap(),
            Mapping {
                root_index: 3,
                wrapper: 0,
                start: 16,
                end: 31,
                reversed: true
            }
        );
        assert_eq!(translate(&[0], 16, 31), Err(Error::NoLaneWindow));
        assert_eq!(translate(&[4], 0, 3), Err(Error::RootIndex(4)));
        assert_eq!(translate(&[0, 0], 0, 3), Err(Error::DuplicateRoot(0)));
        assert_eq!(translate(&[3], 80, 96), Err(Error::CrossesWindow));
        assert_eq!(translate(&[0], 131, 134), Err(Error::NoLaneWindow));
    }
    #[test]
    fn partial_and_descending_reversed_lanes() {
        let m = translate(&[0], 96, 99).unwrap();
        assert_eq!((m.start, m.end, m.reversed), (108, 111, true));
        let m = translate(&[0], 99, 96).unwrap();
        assert_eq!((m.start, m.end, m.reversed), (108, 111, false));
        assert!(translate(&[0], 132, 132).is_ok());
        assert!(translate(&[2], 128, 131).is_ok());
    }
    #[test]
    fn returned_local_port_is_not_the_host_assigned_logical_bridge() {
        let (_, mut ask) = build(&[0], &Port::pcie(0, 3), 0).unwrap();
        ask.status.set(0x0005_0006);
        assert_eq!(ask.status().read(Status::STATE), 6);
        assert_eq!(ask.status().read(Status::LOCAL_PORT_ID), 5);
        assert_eq!(Transport::LOGICAL_BRIDGE.read(ask.transport.get()), 0);
        ask.set_logical_bridge(2).unwrap();
        assert_eq!(ask.status().read(Status::LOCAL_PORT_ID), 5);
        assert_eq!(Transport::LOGICAL_BRIDGE.read(ask.transport.get()), 2);
        assert_eq!(ask.lane().read(Lane::START), 0);
    }
    #[test]
    fn packed_fields_and_validation() {
        let port = Port {
            device: 3,
            function: 4,
            max_speed: LinkSpeed::Gen5,
            target_speed: LinkSpeed::Gen4,
            early_train: true,
            compliance: true,
            hotplug: 5,
            ..Port::pcie(80, 83)
        };
        let (_, mut ask) = build(&[3], &port, 0x1234).unwrap();
        assert_eq!(ask.device.get(), 0x1c);
        assert_eq!(ask.handle.get(), 0x1234_0001);
        assert_eq!(ask.link.get(), 0x1557, "early forces Gen1 target");
        ask.set_logical_bridge(31).unwrap();
        assert_eq!(&ask.bytes()[0x16..0x18], &[31, 0]);
        assert_eq!(ask.set_logical_bridge(32), Err(Error::LogicalBridge(32)));
        assert_eq!(
            build(
                &[0],
                &Port {
                    device: 32,
                    ..Port::pcie(0, 3)
                },
                0
            )
            .unwrap_err(),
            Error::DeviceFunction
        );
        assert_eq!(
            build(&[0], &Port::pcie(0, 2), 0).unwrap_err(),
            Error::Width(3)
        );
        assert_eq!(ask.training_state(), 0);
    }
}
