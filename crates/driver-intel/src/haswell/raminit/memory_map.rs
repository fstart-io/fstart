//! Host-bridge DRAM decode (`memory_map.c`). All quantities here are MiB;
//! only the register programming step converts to physical addresses.

use super::{Error, MchBar};
use fstart_core::mmio::MmioWindow;
use fstart_pci::ecam::EcamDevice;

/// Size of the GDXCBAR MMIO aperture (4 KiB, coreboot `GDXC_BASE_SIZE`).
pub const GDXCBAR_SIZE: u32 = 0x1000;

/// `GGC` (`0x50`) bit 1 (`IVD`): disable IGD VGA I/O and memory decode.
pub const GGC_IVD: u16 = 1 << 1;

/// Compute the host-bridge `GGC` (`0x50`) value before locking.
///
/// When IGD is enabled, reserves 2 MiB GTT (`2 << 8`) and 64 MiB stolen memory
/// (`2 << 3`, in 32 MiB units) with VGA decode enabled (`IVD = 0`).
/// When IGD is disabled, reserves 0 MiB GTT/GMS and sets `IVD` (`0x002`) so
/// VGA decode is not claimed by the disabled IGD.
#[must_use]
pub const fn ggc_value(igd: bool) -> u16 {
    if igd { 0x210 } else { GGC_IVD }
}

/// Clear both GDXC trace reservations (MOT and OCLA), then lock OCLA (not MOT).
pub(crate) fn clear_and_lock_gdxc(gdxc: MmioWindow) {
    for off in [0x10, 0x14, 0x18, 0x20, 0x24, 0x28] {
        gdxc.write32(off, 0);
    }
    // MPCOHTRK_GDXC_OCLA_ADDRESS_HI_LOCK (bit 2).
    gdxc.setbits32(0x24, 1 << 2);
}

#[derive(Clone, Copy, Debug)]
pub struct Reservations {
    pub tseg: u32,
    pub dpr: u32,
    pub me: u32,
    pub igd_primary: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemoryMap {
    pub tom: u32,
    pub tom_minus_me: u32,
    pub tolud: u32,
    pub touud: u32,
    pub remap: Option<(u32, u32)>,
    pub bdsm: u32,
    pub gtt: u32,
    pub tseg: u32,
    pub dpr_base: u32,
}

impl MemoryMap {
    pub fn compute(tom: u32, r: Reservations, igd_enabled: bool) -> Result<Self, Error> {
        let t = tom.checked_sub(r.me).ok_or(Error::InvalidMap)?;
        if !r.tseg.is_power_of_two() || r.dpr > 255 || tom > 0x80000 {
            return Err(Error::InvalidMap);
        }
        let gtt = (t.min(4096) / 2) & !(r.tseg - 1);
        let tseg = gtt.checked_sub(r.tseg).ok_or(Error::InvalidMap)?;
        let dpr_base = tseg.checked_sub(r.dpr).ok_or(Error::InvalidMap)?;
        let igd = igd_enabled || r.igd_primary;
        let bdsm = gtt + if igd { 2 } else { 0 };
        let tolud = bdsm + if igd { 64 } else { 0 };
        if tolud > t {
            return Err(Error::InvalidMap);
        }
        let (touud, remap) = if t > tolud {
            let base = t.max(4096);
            let top = t.min(4096) + base - tolud;
            (top, Some((base, top - 1)))
        } else {
            (t, None)
        };
        Ok(Self {
            tom,
            tom_minus_me: t,
            tolud,
            touud,
            remap,
            bdsm,
            gtt,
            tseg,
            dpr_base,
        })
    }
}

/// # Safety
/// `host` must address Haswell 00:00.0. Its enabled MCHBAR and GDXCBAR
/// must be mapped in the current address space, before the map is locked.
pub unsafe fn configure(
    host: EcamDevice,
    mch: MchBar,
    tom: u32,
    r: Reservations,
) -> Result<MemoryMap, Error> {
    let igd = host.read32(0x54) & (1 << 4) != 0 || r.igd_primary;
    let map = MemoryMap::compute(tom, r, igd)?;
    let gdxc = mch.read32(0x5420);
    if gdxc & 1 == 0 || gdxc & !1 == 0 || gdxc & 3 != 1 {
        return Err(Error::InvalidMap);
    }
    if igd {
        host.or32(0x54, 1 << 4);
    }
    let wide = |off, mib: u32| {
        host.write32(off + 4, mib >> 12);
        host.write32(off, mib << 20);
    };
    wide(0xa8, map.touud);
    wide(0xa0, map.tom);
    if let Some((base, limit)) = map.remap {
        wide(0x90, base);
        wide(0x98, limit);
    }
    if r.me != 0 {
        wide(0x78, 0x80000 - r.me);
        wide(0x70, map.tom_minus_me);
        host.or32(0x78, 1 << 11);
    }
    for (off, val) in [
        (0xbc, map.tolud),
        (0xb0, map.bdsm),
        (0xb4, map.gtt),
        (0xb8, map.tseg),
    ] {
        host.write32(off, val << 20);
    }
    host.write32(0x5c, (map.tseg << 20) | (r.dpr << 4));
    host.write16(0x50, ggc_value(igd));
    let base = (gdxc & !1) as usize;
    // SAFETY: caller guarantees the enabled 4 KiB GDXCBAR is mapped.
    let gdxc_window = unsafe { MmioWindow::new(base, GDXCBAR_SIZE) };
    clear_and_lock_gdxc(gdxc_window);
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remap_excludes_me_and_reserves_graphics() {
        let m = MemoryMap::compute(
            8192,
            Reservations {
                tseg: 8,
                dpr: 4,
                me: 32,
                igd_primary: false,
            },
            true,
        )
        .unwrap();
        assert_eq!(
            (m.dpr_base, m.tseg, m.gtt, m.bdsm, m.tolud),
            (2036, 2040, 2048, 2050, 2114)
        );
        assert_eq!(m.remap, Some((8160, 10141)));
        assert_eq!(m.touud, 10142);
    }
}
