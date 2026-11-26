// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native controller address map, NOT training, activation, or usable RAM.
//!
//! Port of memory_map_native.c (480 lines), SHA256
//! 35cfbf6cebeffd5c2e9cd4f886585d1753bc339f6f18fddcd4a1550e6e554a1b.
//! Intel 332688-003EN pp34–35 distinguishes physical ME backing from logical
//! reclaim: the numeric ME range is NOT another hole in reclaimed CPU RAM.
//! Corrections: checked arithmetic/geometry; TOUUD >=4GiB (p78); no-reclaim
//! explicitly disables remap (pp76–77); disabled MESEG is explicitly cleared;
//! invalid GGC/UMA/PRMRR and configured DPR reject rather than fall back to zero.
use super::{
    capabilities::{Capabilities, CapidA},
    cse::UmaRequest,
    early_sa::{self, EcamConfig, GDXC_BASE},
    registers::{GdxcOclaHigh, GdxcRegisters, MCHBAR_BASE, MchRegisters},
    sa_registers::*,
    spd::Dimms,
    uncore_prmrr,
};
use crate::sunrise_point::pmc::MemoryBootMode;
use fstart_pci::EcamDevice;
use tock_registers::{
    LocalRegisterCopy,
    interfaces::{Readable, Writeable},
};

const MIB: u64 = 1 << 20;
const FOUR_GIB: u64 = 1 << 32;
const ADDRESS: u64 = MemoryBoundary::ADDRESS_MIB.mask << MemoryBoundary::ADDRESS_MIB.shift;
const LOW_ADDRESS: u64 =
    (LowMemoryBoundary::ADDRESS_MIB.mask as u64) << LowMemoryBoundary::ADDRESS_MIB.shift;
const ME_ADDRESS: u64 = MeSegmentBase::ADDRESS_MIB.mask << MeSegmentBase::ADDRESS_MIB.shift;
const LOCK: u64 = MemoryBoundary::LOCK::SET.value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Early(early_sa::Error),
    RetainedMemory,
    InvalidInventory,
    Capabilities,
    InvalidPolicy,
    InvalidGraphics,
    InvalidUma,
    UnsupportedDpr,
    Arithmetic,
    ResourceOverlap,
    UnknownBits(u16),
    Conflict(u16),
    LockedMismatch(u16),
    GdxcInUse,
    Readback(u16),
    Prmrr(uncore_prmrr::Error),
}
impl From<early_sa::Error> for Error {
    fn from(e: early_sa::Error) -> Self {
        Self::Early(e)
    }
}
impl From<uncore_prmrr::Error> for Error {
    fn from(e: uncore_prmrr::Error) -> Self {
        Self::Prmrr(e)
    }
}

/// Source board policy, in MiB; no guessed M900 defaults.
#[derive(Debug, Clone, Copy)]
pub struct Policy {
    pub tseg_mib: u32,
    pub mmio_mib: u32,
}

/// All values are byte addresses: TOM/TOLUD/TOUUD are exclusive tops; other
/// fields are bases. `remap_limit` is the base of the inclusive final MiB
/// (the exact register encoding, not its last byte).
/// This value is a candidate controller geometry, never a DetectedMemory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Plan {
    pub tom: u64,
    pub tolud: u64,
    pub touud: u64,
    pub bdsm: u64,
    pub bgsm: u64,
    pub tseg: u64,
    pub remap_base: u64,
    pub remap_limit: u64,
    /// Physical DRAM backing only. Do not reserve this numeric range again
    /// in the CPU logical high-memory map; reclaim translates it to TOLUD.
    pub me_physical_base: Option<u64>,
    pub me_size_mib: u32,
    pub prmrr: uncore_prmrr::Placement,
}
#[derive(Debug, PartialEq, Eq)]
pub struct ProgrammedMap {
    plan: Plan,
    ecam: EcamConfig,
    ggc: u16,
    dpr: u32,
}
impl ProgrammedMap {
    pub fn plan(&self) -> &Plan {
        &self.plan
    }
    pub fn uncore_prmrr(&self) -> uncore_prmrr::Placement {
        self.plan.prmrr
    }

    /// Read the map back from the programmed controller: what the stages
    /// after training detect.
    pub(crate) fn observe_live(
        sa: &EcamDevice,
        ecam: EcamConfig,
        uma: UmaRequest,
        policy: Policy,
        prm: uncore_prmrr::Request,
    ) -> Result<Self, Error> {
        early_sa::validate_host(sa)?;
        early_sa::require_ecam(sa, ecam)?;
        let total = u32::try_from((MapWord::live(sa, TOM, true) & !LOCK) / MIB)
            .map_err(|_| Error::Arithmetic)?;
        let (ggc, stolen, gtt) = graphics_reservation(sa)?;
        let dpr = require_no_dpr(sa)?;
        let plan = compute(total, u32::from(uma.mib()), stolen, gtt, policy, prm)?;
        if ecam.base < plan.tolud || plan.tolud > MCHBAR_BASE as u64 {
            return Err(Error::ResourceOverlap);
        }
        let observed = Self {
            plan,
            ecam,
            ggc,
            dpr,
        };
        observed.validate_live(sa)?;
        Ok(observed)
    }

    /// Recheck the returned geometry and retained decode identity, allowing only
    /// documented map/GGC/ME/DPR lock transitions. No training, DMA translation
    /// protection or CPU/SMM protection is certified by these register reads.
    /// Caller must retain exclusive access to the actual SA used by program_cold.
    pub fn validate_live(&self, sa: &EcamDevice) -> Result<(), Error> {
        early_sa::validate_host(sa)?;
        early_sa::require_ecam(sa, self.ecam)?;
        if sa.read32(0x48) != MCHBAR_BASE as u32 | 1 || sa.read32(0x4c) != 0 {
            return Err(Error::Early(early_sa::Error::ConflictingDecode));
        }
        let p = &self.plan;
        let me_mask = if p.me_size_mib == 0 {
            0
        } else {
            (MeSegmentMask::MASK_MIB.val(u64::from(p.me_size_mib).wrapping_neg() & 0x7ffff)
                + MeSegmentMask::STOLEN_ENABLE::SET)
                .value
        };
        for (offset, wanted, wide, lock) in [
            (TOM, p.tom, true, LOCK),
            (TOLUD, p.tolud, false, LOCK),
            (TOUUD, p.touud, true, LOCK),
            (REMAP_BASE, p.remap_base, true, LOCK),
            (REMAP_LIMIT, p.remap_limit, true, LOCK),
            (TSEG, p.tseg, false, LOCK),
            (BDSM, p.bdsm, false, LOCK),
            (BGSM, p.bgsm, false, LOCK),
            (MESEG_BASE, p.me_physical_base.unwrap_or(0), true, 0),
            (MESEG_MASK, me_mask, true, MeSegmentMask::LOCK::SET.value),
        ] {
            if MapWord::live(sa, offset, wide) & !lock != wanted {
                return Err(Error::Conflict(offset));
            }
        }
        if sa.read16(GGC) & !1 != self.ggc & !1 {
            return Err(Error::Conflict(GGC));
        }
        if self.ggc & 2 == 0 && sa.read32(DEVEN) & (1 << 4) == 0 {
            return Err(Error::InvalidGraphics);
        }
        if sa.read32(DPR) & !1 != self.dpr & !1 {
            return Err(Error::Conflict(DPR));
        }
        Ok(())
    }
}

fn graphics_reservation(sa: &EcamDevice) -> Result<(u16, u32, u32), Error> {
    let ggc = sa.read16(GGC);
    let graphics = LocalRegisterCopy::<u16, GraphicsControl::Register>::new(ggc);
    let gms = graphics.read(GraphicsControl::STOLEN);
    let devices = LocalRegisterCopy::<u32, DeviceEnable::Register>::new(sa.read32(DEVEN));
    // Intel 332688-003EN p58: disabled IGD requires IVD, even with stale GMS.
    if ggc & 0x38 != 0
        || gms > 16
        || !graphics.is_set(GraphicsControl::VGA_DISABLE)
            && (gms == 0 || !devices.is_set(DeviceEnable::IGD))
    {
        return Err(Error::InvalidGraphics);
    }
    Ok((
        ggc,
        u32::from(gms) * 32,
        [0, 2, 4, 8][graphics.read(GraphicsControl::GTT) as usize],
    ))
}

fn require_no_dpr(sa: &EcamDevice) -> Result<u32, Error> {
    let dpr = LocalRegisterCopy::<u32, DmaProtectedRange::Register>::new(sa.read32(DPR));
    // TOP is RO and follows TSEG; never program it as an independent base.
    if dpr.get() & 0x000f_f008 != 0
        || dpr.read(DmaProtectedRange::SIZE_MIB) != 0
        || dpr.is_set(DmaProtectedRange::ENABLE)
        || dpr.is_set(DmaProtectedRange::STATUS)
    {
        return Err(Error::UnsupportedDpr);
    }
    Ok(dpr.get())
}

pub(super) fn compute(
    total: u32,
    me: u32,
    stolen: u32,
    gtt: u32,
    policy: Policy,
    prm: uncore_prmrr::Request,
) -> Result<Plan, Error> {
    if total == 0
        || total >= 0x80000
        || me > total
        || me != 0 && (!me.is_power_of_two() || (total - me) & (me - 1) != 0)
    {
        return Err(Error::InvalidUma);
    }
    if policy.mmio_mib == 0
        || policy.mmio_mib >= 4096
        || policy.tseg_mib != 0 && !policy.tseg_mib.is_power_of_two()
    {
        return Err(Error::InvalidPolicy);
    }
    let available = i64::from(total - me);
    let initial_tolud = available.min(4096 - i64::from(policy.mmio_mib));
    let mut tolud = initial_tolud;
    let mut bdsm = tolud - i64::from(stolen);
    let mut bgsm = bdsm - i64::from(gtt);
    let mut tseg = bgsm - i64::from(policy.tseg_mib);
    let size = i64::from(prm.size_mib());
    let mut align = 0;
    let prm_base = if size >= 32 {
        let org = tseg - size;
        let base = org & !127;
        align = org - base;
        base
    } else if size == 1 {
        let misalign = if policy.tseg_mib == 0 {
            0
        } else {
            (tseg & -i64::from(policy.tseg_mib)) - tseg
        };
        tseg - 1 + misalign
    } else {
        tseg
    };
    if policy.tseg_mib != 0 {
        let org = tseg - align;
        let mut new = org & -i64::from(policy.tseg_mib);
        if new != org && size > 1 {
            new += i64::from(policy.tseg_mib);
        }
        let shift = new - org - align;
        tseg = new;
        bgsm += shift;
        bdsm += shift;
        tolud += shift;
    }
    let prm_base = if size == 0 { tseg } else { prm_base };
    if prm_base < 1
        || tseg < 1
        || tolud > initial_tolud
        || tolud > 4095
        || prm_base + size > tseg
        || tseg + i64::from(policy.tseg_mib) != bgsm
        || bgsm + i64::from(gtt) != bdsm
        || bdsm + i64::from(stolen) != tolud
    {
        return Err(Error::Arithmetic);
    }
    let tolud = tolud as u64 * MIB;
    let available = available as u64 * MIB;
    let (remap_base, remap_limit, touud) = if available > tolud {
        let base = FOUR_GIB.max(available);
        let top = base
            .checked_add(FOUR_GIB.min(available) - tolud)
            .ok_or(Error::Arithmetic)?;
        (base, top - MIB, top)
    } else {
        // Documented reset geometry disables the window, unlike native 0/0.
        (ADDRESS, 0, FOUR_GIB)
    };
    if touud > ADDRESS
        || remap_base > ADDRESS
        || remap_limit > ADDRESS
        || touud > prm.address_limit()
        || u64::from(total) * MIB > prm.address_limit()
    {
        return Err(Error::Arithmetic);
    }
    Ok(Plan {
        tom: u64::from(total) * MIB,
        tolud,
        touud: touud.max(FOUR_GIB),
        bdsm: bdsm as u64 * MIB,
        bgsm: bgsm as u64 * MIB,
        tseg: tseg as u64 * MIB,
        remap_base,
        remap_limit,
        me_physical_base: if me == 0 { None } else { Some(available) },
        me_size_mib: me,
        prmrr: prm.placement(prm_base as u32)?,
    })
}

fn inventory(sa: &EcamDevice, dimms: &Dimms<'_>, caps: Capabilities) -> Result<u32, Error> {
    if [caps.capid_a, caps.capid_b, caps.capid_c].contains(&u32::MAX)
        || [sa.read32(0xe4), sa.read32(0xe8), sa.read32(0xec)]
            != [caps.capid_a, caps.capid_b, caps.capid_c]
        || caps.capid_b & 8 == 0
    {
        return Err(Error::Capabilities);
    }
    let a = LocalRegisterCopy::<u32, CapidA::Register>::new(caps.capid_a);
    let max = match a.read(CapidA::MAX_CHANNEL_SIZE) {
        0 => 32768,
        1 => 8192,
        2 => 4096,
        _ => 2048,
    };
    let mut total = 0u32;
    let mut channels = 0;
    for channel in dimms {
        let mut capacity = 0u32;
        let mut count = 0;
        for d in channel.iter().flatten() {
            // Keep the actual decoded inventory; never accept a forged size.
            if super::spd::Dimm::decode(d.raw).map_err(|_| Error::InvalidInventory)? != *d {
                return Err(Error::InvalidInventory);
            }
            capacity = capacity.checked_add(d.size_mib).ok_or(Error::Arithmetic)?;
            count += 1;
        }
        if capacity > max || a.is_set(CapidA::ONE_DIMM_PER_CHANNEL) && count > 1 {
            return Err(Error::Capabilities);
        }
        channels += u8::from(count != 0);
        total = total.checked_add(capacity).ok_or(Error::Arithmetic)?;
    }
    if total == 0 || a.is_set(CapidA::SINGLE_CHANNEL) && channels > 1 {
        return Err(Error::InvalidInventory);
    }
    Ok(total)
}

#[derive(Clone, Copy)]
struct MapWord {
    offset: u16,
    wanted: u64,
    allowed: u64,
    lock: u64,
    wide: bool,
    reset: u64,
    replace_unlocked: bool,
}
impl MapWord {
    fn live(sa: &EcamDevice, offset: u16, wide: bool) -> u64 {
        u64::from(sa.read32(offset))
            | if wide {
                u64::from(sa.read32(offset + 4)) << 32
            } else {
                0
            }
    }
    fn preflight(self, sa: &EcamDevice) -> Result<(), Error> {
        let live = Self::live(sa, self.offset, self.wide);
        if live & !self.allowed != 0 {
            return Err(Error::UnknownBits(self.offset));
        }
        if live & self.lock != 0 {
            if live & !self.lock != self.wanted {
                return Err(Error::LockedMismatch(self.offset));
            }
        } else if !self.replace_unlocked && live != self.wanted && live != self.reset {
            return Err(Error::Conflict(self.offset));
        }
        Ok(())
    }
    fn program(self, sa: &EcamDevice) -> Result<(), Error> {
        let live = Self::live(sa, self.offset, self.wide);
        if live & self.lock == 0 && live != self.wanted {
            // Native config64 is LOW DWORD then HIGH DWORD, not a u64 store.
            if self.wide {
                sa.write32(self.offset, self.wanted as u32);
                sa.write32(self.offset + 4, (self.wanted >> 32) as u32);
            } else {
                unsafe {
                    sa.register::<u32, LowMemoryBoundary::Register>(self.offset)
                        .set(self.wanted as u32);
                }
            }
        }
        self.verify(sa, live & self.lock)
    }
    fn verify(self, sa: &EcamDevice, preserved_lock: u64) -> Result<(), Error> {
        if Self::live(sa, self.offset, self.wide) != self.wanted | preserved_lock {
            return Err(Error::Readback(self.offset));
        }
        Ok(())
    }
}

/// Programs controller geometry only, with whole-phase preflight and exact
/// readback. No map locks are newly asserted here (native MC-done owns those).
/// Matching locked values are preserved without writing under a lock.
///
/// # Safety
/// Own the cold controller/config/MCH/GDXC phase exclusively. `mch` and `gdxc`
/// must be operational mappings of the actual source fixed physical resources
/// (virtual aliases allowed). `dimms`, fuse capabilities, PRM policy/request and
/// the CSE request must originate from actual device reads, not test defaults.
/// The request must be the completed genuine CSE firmware wait result.
/// No retained DRAM, active traffic or previously activated RAM is permitted.
/// All other platform resources must lie outside this candidate RAM geometry;
/// fixed decode/ECAM overlaps are checked here. Errors after the first write
/// are fatal partial configuration; there is no hardware rollback claim.
#[allow(clippy::too_many_arguments)]
pub unsafe fn program_cold(
    sa: &EcamDevice,
    mch: &MchRegisters,
    gdxc: &GdxcRegisters,
    ecam: EcamConfig,
    mode: MemoryBootMode,
    dimms: &Dimms<'_>,
    caps: Capabilities,
    uma: UmaRequest,
    policy: Policy,
    prm: uncore_prmrr::Request,
) -> Result<ProgrammedMap, Error> {
    if !matches!(mode, MemoryBootMode::Cold | MemoryBootMode::S3) {
        return Err(Error::RetainedMemory);
    }
    early_sa::validate_host(sa)?;
    early_sa::require_ecam(sa, ecam)?;
    if sa.read32(0x48) != MCHBAR_BASE as u32 | 1
        || sa.read32(0x4c) != 0
        || mch.gdxc_bar.get() != GDXC_BASE | 1
        || mch.gdxc_bar_high.get() != 0
    {
        return Err(Error::Early(early_sa::Error::ConflictingDecode));
    }
    let total = inventory(sa, dimms, caps)?;
    let (ggc, stolen, gtt) = graphics_reservation(sa)?;
    let dpr = require_no_dpr(sa)?;
    let plan = compute(total, u32::from(uma.mib()), stolen, gtt, policy, prm)?;
    if ecam.base < plan.tolud || plan.tolud > MCHBAR_BASE as u64 {
        return Err(Error::ResourceOverlap);
    }
    // Coreboot's unused-GDXC path programs only OCLA_ADDRESS_HI. The
    // other words are not evidence of an active reservation or required to
    // read zero. An already-locked incompatible value cannot be replaced.
    let ocla_high = gdxc.ocla_address_high.extract();
    if ocla_high.is_set(GdxcOclaHigh::LOCK) && ocla_high.get() != GdxcOclaHigh::LOCK::SET.value {
        return Err(Error::GdxcInUse);
    }
    let me_mask = if plan.me_size_mib == 0 {
        0
    } else {
        MeSegmentMask::MASK_MIB
            .val(u64::from(plan.me_size_mib).wrapping_neg() & 0x7ffff)
            .value
            | MeSegmentMask::STOLEN_ENABLE::SET.value
    };
    let me_lock = MeSegmentMask::LOCK::SET.value;
    let live_me_mask =
        u64::from(sa.read32(MESEG_MASK)) | u64::from(sa.read32(MESEG_MASK + 4)) << 32;
    let me_base = plan.me_physical_base.unwrap_or(0);
    let words = [
        MapWord {
            offset: TOM,
            wanted: plan.tom,
            allowed: ADDRESS | LOCK,
            lock: LOCK,
            wide: true,
            reset: ADDRESS,
            replace_unlocked: false,
        },
        MapWord {
            offset: TOLUD,
            wanted: plan.tolud,
            allowed: LOW_ADDRESS | LOCK,
            lock: LOCK,
            wide: false,
            reset: MIB,
            replace_unlocked: false,
        },
        MapWord {
            offset: TOUUD,
            wanted: plan.touud,
            allowed: ADDRESS | LOCK,
            lock: LOCK,
            wide: true,
            reset: 0,
            replace_unlocked: false,
        },
        MapWord {
            offset: REMAP_BASE,
            wanted: plan.remap_base,
            allowed: ADDRESS | LOCK,
            lock: LOCK,
            wide: true,
            reset: ADDRESS,
            replace_unlocked: plan.remap_base > plan.remap_limit,
        },
        MapWord {
            offset: REMAP_LIMIT,
            wanted: plan.remap_limit,
            allowed: ADDRESS | LOCK,
            lock: LOCK,
            wide: true,
            reset: 0,
            replace_unlocked: plan.remap_base > plan.remap_limit,
        },
        MapWord {
            offset: TSEG,
            wanted: plan.tseg,
            allowed: LOW_ADDRESS | LOCK,
            lock: LOCK,
            wide: false,
            reset: 0,
            replace_unlocked: false,
        },
        MapWord {
            offset: BDSM,
            wanted: plan.bdsm,
            allowed: LOW_ADDRESS | LOCK,
            lock: LOCK,
            wide: false,
            reset: 0,
            replace_unlocked: false,
        },
        MapWord {
            offset: BGSM,
            wanted: plan.bgsm,
            allowed: LOW_ADDRESS | LOCK,
            lock: LOCK,
            wide: false,
            reset: MIB,
            replace_unlocked: false,
        },
        // Source order is MASK before BASE, and MELCK locks BOTH words.
        MapWord {
            offset: MESEG_MASK,
            wanted: me_mask,
            allowed: ADDRESS | me_lock | MeSegmentMask::STOLEN_ENABLE::SET.value,
            lock: me_lock,
            wide: true,
            reset: 0,
            replace_unlocked: false,
        },
        MapWord {
            offset: MESEG_BASE,
            wanted: me_base,
            allowed: ME_ADDRESS,
            lock: 0,
            wide: true,
            reset: ADDRESS,
            replace_unlocked: live_me_mask == 0,
        },
    ];
    for word in words {
        word.preflight(sa)?;
    }
    if live_me_mask & me_lock != 0 && MapWord::live(sa, MESEG_BASE, true) != me_base {
        return Err(Error::LockedMismatch(MESEG_BASE));
    }
    for (i, word) in words.into_iter().enumerate() {
        if i == 9 && live_me_mask & me_lock != 0 {
            continue;
        }
        word.program(sa)?;
    }
    if !ocla_high.is_set(GdxcOclaHigh::LOCK) {
        gdxc.ocla_address_high.write(GdxcOclaHigh::LOCK::SET);
    }
    if gdxc.ocla_address_high.get() != GdxcOclaHigh::LOCK::SET.value {
        return Err(Error::Readback(0x24));
    }
    for word in words {
        if MapWord::live(sa, word.offset, word.wide) & !word.lock != word.wanted {
            return Err(Error::Readback(word.offset));
        }
    }
    if sa.read16(GGC) != ggc || sa.read32(DPR) & 0xfffff != dpr & 0xfffff {
        return Err(Error::Conflict(DPR));
    }
    Ok(ProgrammedMap {
        plan,
        ecam,
        ggc,
        // TOP is read-only and follows newly programmed TSEG.
        dpr: sa.read32(DPR),
    })
}

#[cfg(test)]
#[path = "memory_map_tests.rs"]
mod tests;
