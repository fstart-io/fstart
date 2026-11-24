// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native hsio.c reference recipes, FIA ownership and CSE ChipsetInit protocol.

use super::{
    dmi,
    pcr::{self, Pcr, Port},
    pmc::MemoryBootMode,
};
use crate::skylake::cse::{self, MmioCse};
use embedded_hal::delay::DelayNs;
use fstart_pci::EcamDevice;
use tock_registers::{
    LocalRegisterCopy,
    interfaces::{ReadWriteable, Readable, Writeable},
    register_bitfields,
};

mod data;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum Owner {
    PcieDmi = 0,
    Usb3 = 1,
    Sata = 2,
    Gbe = 3,
    Ssic = 5,
    Common = 16,
    Broadcast = 17,
}
struct Entry {
    port: Port,
    value: u32,
    offset: u16,
    owner: Owner,
}

register_bitfields![u16,
    Address [
        GROUP OFFSET(9) NUMBITS(5) [],
        ACCESS OFFSET(14) NUMBITS(2) []
    ]
];
register_bitfields![u32,
    LaneOwners [
        L0 OFFSET(0) NUMBITS(4) [],
        L1 OFFSET(4) NUMBITS(4) [],
        L2 OFFSET(8) NUMBITS(4) [],
        L3 OFFSET(12) NUMBITS(4) [],
        L4 OFFSET(16) NUMBITS(4) [],
        L5 OFFSET(20) NUMBITS(4) [],
        L6 OFFSET(24) NUMBITS(4) [],
        L7 OFFSET(28) NUMBITS(4) []
    ],
    PcsSignal [ STATE OFFSET(14) NUMBITS(2) [] ],
    TxPinState [ L1_EXIT OFFSET(25) NUMBITS(1) [] ],
    PcsControl [ BIT31 OFFSET(31) NUMBITS(1) [] ],
    RxWander [ ENABLE OFFSET(29) NUMBITS(1) [] ]
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Family {
    Lp,
    SklH,
    KblH,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reference {
    LpBx,
    LpCx,
    SklHBx,
    SklHDx,
    KblHAx,
}
impl Reference {
    fn entries(self) -> &'static [Entry] {
        match self {
            Self::LpBx => data::PCH_LP_HSIO_BX,
            Self::LpCx => data::PCH_LP_HSIO_CX,
            Self::SklHBx => data::PCH_H_HSIO_BX,
            Self::SklHDx => data::PCH_H_HSIO_DX,
            Self::KblHAx => data::KBL_PCH_H_HSIO_AX,
        }
    }
    fn blob(self) -> &'static [u8] {
        match self {
            Self::LpBx => data::PCH_LP_CHIPSET_INIT_BX,
            Self::LpCx => data::PCH_LP_CHIPSET_INIT_CX,
            Self::SklHBx => data::PCH_H_CHIPSET_INIT_BX,
            Self::SklHDx => data::PCH_H_CHIPSET_INIT_DX,
            Self::KblHAx => data::KBL_PCH_H_CHIPSET_INIT_AX,
        }
    }
    fn crc(self) -> u16 {
        u16::from_le_bytes([self.blob()[0], self.blob()[1]])
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Device,
    UnsupportedSilicon,
    UnsupportedStepping(u8),
    RetainedMemory,
    Lane,
    Pcr(pcr::Error),
    Cse(cse::Error),
    IdentityChanged,
}
impl From<pcr::Error> for Error {
    fn from(error: pcr::Error) -> Self {
        Self::Pcr(error)
    }
}

/// Actual LPC identity, not a CPU-generation assumption: KBL CPUs also ship
/// with a 100-series PCH. The private fields are populated only by PCI reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Silicon {
    device_id: u16,
    revision: u8,
    family: Family,
}

#[derive(Debug, Clone, Copy)]
pub struct Lane {
    pub port: Port,
    pub base: u16,
}
impl Silicon {
    pub fn read(lpc: &EcamDevice) -> Result<Self, Error> {
        if lpc.address().segment() != 0
            || lpc.bus() != 0
            || lpc.dev() != 0x1f
            || lpc.func() != 0
            || lpc.try_read16(0x00).ok_or(Error::Device)? != 0x8086
            || lpc.try_read16(0x0a).ok_or(Error::Device)? != 0x0601
        {
            return Err(Error::Device);
        }
        let device_id = lpc.try_read16(0x02).ok_or(Error::Device)?;
        let family = match device_id {
            0x9d40..=0x9d5f => Family::Lp,
            0xa140..=0xa15f => Family::SklH,
            0xa2c0..=0xa2d4 => Family::KblH,
            _ => return Err(Error::UnsupportedSilicon),
        };
        Ok(Self {
            device_id,
            revision: lpc.try_read8(0x08).ok_or(Error::Device)?,
            family,
        })
    }
    pub fn kind(self) -> dmi::Kind {
        if self.family == Family::Lp {
            dmi::Kind::Lp
        } else {
            dmi::Kind::H
        }
    }
    pub fn is_kbl_h(self) -> bool {
        self.family == Family::KblH
    }
    pub fn max_lane(self) -> u8 {
        match self.family {
            Family::Lp => 15,
            Family::SklH => 29,
            Family::KblH => 33,
        }
    }
    pub fn lane(self, lane: u8) -> Result<Lane, Error> {
        if lane > self.max_lane() {
            return Err(Error::Lane);
        }
        let (start, port) = if self.family == Family::Lp {
            match lane {
                0..=5 => (0, Port::Modphy0),
                6..=9 => (6, Port::Modphy1),
                _ => (10, Port::Modphy2),
            }
        } else {
            match lane {
                0..=5 => (0, Port::Modphy0),
                6..=13 => (6, Port::Modphy1),
                14..=21 => (14, Port::Modphy2),
                22..=29 => (22, Port::Modphy3),
                _ => (30, Port::Modphy4),
            }
        };
        Ok(Lane {
            port,
            base: u16::from(lane - start) * 0x200,
        })
    }
    pub fn lane_owner(self, pcr: &Pcr, lane: u8) -> Result<u8, Error> {
        if lane > self.max_lane() {
            return Err(Error::Lane);
        }
        // KBL-H's four PCIe-only lanes have no FIA LOS ownership bits.
        if self.family == Family::KblH && lane >= 30 {
            return Ok(0);
        }
        let register =
            pcr.register::<LaneOwners::Register>(Port::Fia, 0x250 + u16::from(lane / 8) * 4)?;
        let fields = [
            LaneOwners::L0,
            LaneOwners::L1,
            LaneOwners::L2,
            LaneOwners::L3,
            LaneOwners::L4,
            LaneOwners::L5,
            LaneOwners::L6,
            LaneOwners::L7,
        ];
        Ok(register.read(fields[usize::from(lane % 8)]) as u8)
    }
    fn reference(self) -> Result<Reference, Error> {
        match (self.family, self.revision) {
            (Family::Lp, 0x10 | 0x11) => Ok(Reference::LpBx),
            (Family::Lp, 0x20 | 0x21) => Ok(Reference::LpCx),
            (Family::SklH, 0x10 | 0x20) => Ok(Reference::SklHBx),
            (Family::SklH, 0x30 | 0x31) => Ok(Reference::SklHDx),
            (Family::KblH, 0) => Ok(Reference::KblHAx),
            _ => Err(Error::UnsupportedStepping(self.revision)),
        }
    }
    fn pin_state_supported(self) -> bool {
        self.family == Family::KblH
            || matches!(
                self.device_id,
                0xa152 | 0xa153 | 0xa154 | 0xa156 | 0x9d4b | 0x9d4e | 0x9d50
            )
    }
    fn has_owner(self, pcr: &Pcr, port: Port, owner: Owner) -> Result<bool, Error> {
        let h = self.family != Family::Lp;
        let (first, last) = match port {
            Port::Modphy0 => (0, 5),
            Port::Modphy1 => (6, if h { 13 } else { 9 }),
            Port::Modphy2 => (if h { 14 } else { 10 }, if h { 21 } else { 15 }),
            Port::Modphy3 if h => (22, 29),
            Port::Modphy4 => return Ok(owner == Owner::PcieDmi),
            _ => return Ok(true),
        };
        for lane in first..=last {
            if self.lane_owner(pcr, lane)? == owner as u8 {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

/// Observation of the actual CRC query and the writes made by pre_memory.
/// A ChipsetInit CRC is a version tag, not authentication or analog training.
#[derive(Debug, Clone, Copy)]
pub struct PreMemory {
    silicon: Silicon,
    reference: Reference,
    me_crc: Option<u16>,
}
impl PreMemory {
    pub fn dmi_gen1_only(self) -> bool {
        self.me_crc.is_some_and(|crc| crc != self.reference.crc())
    }
    pub fn kind(self) -> dmi::Kind {
        self.silicon.kind()
    }
    pub fn validate_current(self, lpc: &EcamDevice) -> Result<(), Error> {
        if Silicon::read(lpc)? != self.silicon {
            return Err(Error::IdentityChanged);
        }
        Ok(())
    }
}

/// Execute before any IP or SA DMI initialization. When CSE firmware policy
/// refuses the HSIO CRC query (such as ME Debug mode), apply the reference
/// table directly as coreboot's native hsio.c does. Never run on retained RAM.
pub fn pre_memory(
    lpc: &EcamDevice,
    pcr: &Pcr,
    cse: &mut MmioCse,
    mode: MemoryBootMode,
    delay: &mut impl DelayNs,
) -> Result<PreMemory, Error> {
    if mode != MemoryBootMode::Cold {
        return Err(Error::RetainedMemory);
    }
    let silicon = Silicon::read(lpc)?;
    let reference = silicon.reference()?;
    let status = cse.status().map_err(Error::Cse)?;
    let me_crc = if status.allows(cse::Command::HsioCrc) {
        cse.initialize(delay).map_err(Error::Cse)?;
        let (crc, _, _) = cse.hsio_crc(delay).map_err(Error::Cse)?;
        Some(crc)
    } else {
        fstart_log::warn!(
            "HSIO: CSE path {:?} does not allow HSIO CRC; applying reference table",
            fstart_log::DebugValue(&status.bios_path())
        );
        None
    };
    let result = PreMemory {
        silicon,
        reference,
        me_crc,
    };
    if result.dmi_gen1_only() {
        return Ok(result);
    }
    for entry in reference.entries() {
        let address = LocalRegisterCopy::<u16, Address::Register>::new(entry.offset);
        if address.read(Address::ACCESS) == 2
            && address.read(Address::GROUP) != 0
            && !silicon.has_owner(pcr, entry.port, entry.owner)?
        {
            continue;
        }
        pcr.register::<()>(entry.port, entry.offset)?
            .set(entry.value);
    }
    if silicon.family == Family::KblH {
        pcr.register::<()>(Port::Modphy3, 0x8238)?.set(0x7a);
    }
    if silicon.kind() == dmi::Kind::H {
        for lane in 14..=17 {
            let lane = silicon.lane(lane)?;
            let pcs = pcr.register::<PcsControl::Register>(lane.port, lane.base)?;
            pcr.register::<RxWander::Register>(lane.port, lane.base + 0x13c)?
                .modify(RxWander::ENABLE.val(u32::from(!pcs.is_set(PcsControl::BIT31))));
        }
    }
    if silicon.pin_state_supported() {
        for number in 0..=silicon.max_lane() {
            if silicon.lane_owner(pcr, number)? != Owner::PcieDmi as u8 {
                continue;
            }
            let lane = silicon.lane(number)?;
            pcr.register::<PcsSignal::Register>(lane.port, lane.base + 0x004)?
                .modify(PcsSignal::STATE.val(3));
            pcr.register::<TxPinState::Register>(lane.port, lane.base + 0x0dc)?
                .modify(TxPinState::L1_EXIT::SET);
        }
    }
    Ok(result)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncOutcome {
    InSync,
    ColdResetRequired,
}

/// Post-memory ChipsetInit synchronization. An acknowledged write REQUIRES the
/// source cold reset; it is not permission to continue running the current boot.
/// Request/response storage is supplied by the DRAM-stage owner, never global.
pub fn post_memory(
    lpc: &EcamDevice,
    cse: &mut MmioCse,
    request: &mut [u8],
    response: &mut [u8],
    delay: &mut impl DelayNs,
) -> Result<SyncOutcome, Error> {
    let reference = Silicon::read(lpc)?.reference()?;
    let status = cse.status().map_err(Error::Cse)?;
    if !status.allows(cse::Command::HsioCrc) {
        fstart_log::warn!(
            "HSIO post-memory: CSE path {:?} does not allow HSIO sync; skipping",
            fstart_log::DebugValue(&status.bios_path())
        );
        return Ok(SyncOutcome::InSync);
    }
    cse.initialize(delay).map_err(Error::Cse)?;
    let (me_crc, _, _) = cse.hsio_crc(delay).map_err(Error::Cse)?;
    if me_crc == reference.crc() {
        return Ok(SyncOutcome::InSync);
    }
    // SAFETY: these are the trusted source literals for the actual LPC silicon.
    unsafe { cse.write_hsio_settings(reference.blob(), request, response, delay) }
        .map_err(Error::Cse)?;
    Ok(SyncOutcome::ColdResetRequired)
}
