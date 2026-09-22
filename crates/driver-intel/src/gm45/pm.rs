//! GM45 post-training power setup, coreboot gm45/pm.c `init_pm(..., 0)`.
//! Frequency-scaling tables are deliberately not invoked by this romstage path.
use super::IntelGm45;
use fstart_core::mmio::MmioReadWrite;
use fstart_core::services::ServiceError;
use tock_registers::interfaces::{Readable, Writeable};
use tock_registers::register_structs;

register_structs! {
    PmRegs {
        (0x000 => _pad0: [u8; 0x40]),
        (0x040 => r40: MmioReadWrite<u32>),
        (0x044 => r44: MmioReadWrite<u32>),
        (0x048 => r48: MmioReadWrite<u16>),
        (0x04a => _pad1: [u8; 0x26]),
        (0x070 => r70: MmioReadWrite<u32>),
        (0x074 => _pad2: [u8; 1]),
        (0x075 => r75: MmioReadWrite<u8>),
        (0x076 => _pad3: [u8; 1]),
        (0x077 => r77: MmioReadWrite<u8>),
        (0x078 => _pad4: [u8; 0x18]),
        (0x090 => r90: MmioReadWrite<u32>),
        (0x094 => r94: MmioReadWrite<u32>),
        (0x098 => _pad5: [u8; 0x38]),
        (0x0d0 => rd0: MmioReadWrite<u8>),
        (0x0d1 => _pad6: [u8; 0xa2f]),
        (0xb00 => rb00: MmioReadWrite<u8>),
        (0xb01 => _pad7: [u8; 0xd7]),
        (0xbd8 => rbd8: MmioReadWrite<u8>),
        (0xbd9 => _pad8: [u8; 0x3b]),
        (0xc14 => rc14: MmioReadWrite<u16>),
        (0xc16 => rc16: MmioReadWrite<u16>),
        (0xc18 => _pad9: [u8; 4]),
        (0xc1c => scratch: MmioReadWrite<u16>),
        (0xc1e => _pad10: [u8; 2]),
        (0xc20 => rc20: MmioReadWrite<u16>),
        (0xc22 => _pad11: [u8; 0x16]),
        (0xc38 => rc38: MmioReadWrite<u16>),
        (0xc3a => rc3a: MmioReadWrite<u16>),
        (0xc3c => _pad12: [u8; 0x2c4]),
        (0xf00 => rf00: MmioReadWrite<u32>),
        (0xf04 => rf04: MmioReadWrite<u32>),
        (0xf08 => rf08: MmioReadWrite<u16>),
        (0xf0a => _pad13: [u8; 2]),
        (0xf0c => rf0c: MmioReadWrite<u16>),
        (0xf0e => _pad14: [u8; 2]),
        (0xf10 => rf10: MmioReadWrite<u8>),
        (0xf11 => _pad15: [u8; 0x4f]),
        (0xf60 => rf60: MmioReadWrite<u32>),
        (0xf64 => _pad16: [u8; 0x1c]),
        (0xf80 => rf80: MmioReadWrite<u32>),
        (0xf84 => _pad17: [u8; 0x2c]),
        (0xfb0 => rfb0: MmioReadWrite<u8>),
        (0xfb1 => _pad18: [u8; 7]),
        (0xfb8 => rfb8: MmioReadWrite<u8>),
        (0xfb9 => _pad19: [u8; 7]),
        (0xfc0 => rfc0: MmioReadWrite<u32>),
        (0xfc4 => _pad20: [u8; 0x38]),
        (0xffc => rffc: MmioReadWrite<u16>),
        (0xffe => _pad21: [u8; 0x202]),
        (0x1200 => @END),
    }
}

fn update32(reg: &MmioReadWrite<u32>, clear: u32, set: u32) {
    reg.set((reg.get() & !clear) | set);
}
fn update16(reg: &MmioReadWrite<u16>, clear: u16, set: u16) {
    reg.set((reg.get() & !clear) | set);
}
fn update8(reg: &MmioReadWrite<u8>, clear: u8, set: u8) {
    reg.set((reg.get() & !clear) | set);
}

pub(super) fn fsb_values(fsb: u32) -> Option<(u16, u8, u32, u32, u16)> {
    match fsb {
        667 => Some((0x0ea6, 0x0e, 0x600, 0x1d80, 0x0b96)),
        800 => Some((0x1194, 0x10, 0x700, 0x2380, 0x0c99)),
        1067 => Some((0x1777, 0x15, 0x900, 0x2e80, 0x10a4)),
        _ => None,
    }
}

impl IntelGm45 {
    pub(super) fn post_dram_pm(
        &self,
        revision: u8,
        fsb: u32,
        mem_mhz: u32,
    ) -> Result<(), ServiceError> {
        let (frequency, c16, f00, f04, f0c) = fsb_values(fsb).ok_or(ServiceError::HardwareError)?;
        if !matches!(revision, 5 | 6 | 7 | 9) || !matches!(mem_mhz, 333 | 400 | 533) {
            return Err(ServiceError::HardwareError);
        }
        #[cfg(target_arch = "x86_64")]
        let cores = {
            if core::arch::x86_64::__cpuid(0).eax < 4 {
                return Err(ServiceError::HardwareError);
            }
            ((core::arch::x86_64::__cpuid_count(4, 0).eax >> 26) & 0x3f) + 1
        };
        #[cfg(not(target_arch = "x86_64"))]
        let cores = return Err(ServiceError::HardwareError);
        if !matches!(cores, 2 | 4) {
            return Err(ServiceError::HardwareError);
        }
        // SAFETY: MCHBAR is enabled by setup_early_decode and validated at construction.
        let r = unsafe { &*(self.config.mchbar as *const PmRegs) };
        r.rc14.set(0);
        r.rc20.set(0);
        r.rfc0.set(0x001f_00fd);
        update32(&r.rfc0, 0, (3 << 25) | (1 << 11));
        r.rfb0.set(3);
        update8(&r.rf10, 0, 2);
        r.rc3a.set(frequency);
        update8_byte(&r.rc16, 0x7f, c16);
        r.rfb8.set(3);
        r.rc38.set(frequency);
        update8(&r.rf10, 0, 1 << 5);
        update16(&r.rc16, 0, 3 << 12);
        r.rf60.set(0x0103_0419);
        r.rf00.set(f00);
        r.rf04.set(f04);
        r.rf08.set(0x730f);
        r.rf0c.set(f0c);
        update32(&r.rf80, 0, 1 << 31);
        update32(
            &r.r40,
            0x3f << 24 | 1 << 19,
            if cores == 4 { 1 << 24 } else { 0 } | (1 << 13) | (1 << 21) | (1 << 9),
        );
        update32(
            &r.r70,
            (if revision > 5 { 1 << 30 } else { 0 }) | (1 << 29) | (3 << 21) | (1 << 16),
            (if revision > 5 && fsb != 1067 {
                1 << 30
            } else {
                0
            }) | (if revision > 5 {
                (1 << 28) | (1 << 25)
            } else {
                0
            }) | (if fsb != 667 { 1 << 21 } else { 0 })
                | (1 << 3)
                | (if revision == 7 || revision == 9 { 1 } else { 0 }),
        );
        update32(
            &r.r44,
            3 << 24,
            (1 << 31)
                | (if cores == 2 { 1 << 26 } else { 0 })
                | (1 << 21)
                | (2 << 24)
                | (3 << 4)
                | (if revision == 7 || revision == 9 {
                    1 << 30
                } else {
                    0
                }),
        );
        update32(&r.r90, 7, 4);
        update32(
            &r.r94,
            0,
            (1 << 29)
                | (1 << 11)
                | (if revision == 7 || revision == 9 {
                    1 << 21
                } else {
                    0
                }),
        );
        update8(&r.rb00, 1, 1 << 7);
        update8(
            &r.r75,
            if fsb == 1067 { !2u8 } else { 0 },
            if fsb == 1067 { 0 } else { 1 << 6 },
        );
        update8(
            &r.r77,
            0,
            3 | (1 << 2)
                | (if revision == 7 || revision == 9 {
                    1 << 4
                } else {
                    0
                }),
        );
        if r.r90.get() & (1 << 8) != 0 {
            update32(&r.r90, 7 << 4, 0);
        }
        update8(&r.rd0, 0, 1 << 1);
        update8(&r.rbd8, 0, 3 << 2);
        if revision == 7 || revision == 9 {
            update16(&r.rc14, 0, 1 << 1);
        }
        update16(&r.rffc, 0x7ff, 0x7c0);
        update16(&r.r48, 0xff << 2, 0xaa << 2);
        if revision == 9 {
            update32(&r.r40, 0, 1 << 12);
            update32(&r.r94, 0, 3 << 22);
        }
        #[cfg(target_arch = "x86_64")]
        let super_lfm = unsafe { fstart_arch::x86::msr::rdmsr(0xee) } & (1 << 27) != 0;
        #[cfg(not(target_arch = "x86_64"))]
        let super_lfm = false;
        // SAFETY: fixed CLKCFG register within enabled MCHBAR.
        let clock = unsafe { &*(self.config.mchbar as *const super::Gm45MchClockRegs) };
        let mut value = clock.clkcfg.get();
        if super_lfm {
            value = (value & !(1 << 7)) | (1 << 14);
        } else {
            value = (value & !(1 << 14)) | (1 << 7);
            update32(&r.r44, 1 << 31, 0);
        }
        if fsb == 1067 && mem_mhz == 333 {
            value &= !(1 << 17);
        }
        // The last CLKCFG write is the memory-clock selection, per pm.c.
        clock.clkcfg.set(value);
        Ok(())
    }

    pub(super) fn mark_post_dram(&self) {
        // SAFETY: enabled MCHBAR; marker is written only after VC1 negotiation.
        let r = unsafe { &*(self.config.mchbar as *const PmRegs) };
        r.scratch.set(0xcafe);
    }

    pub(super) fn enable_b3_iommu_workaround(&self) {
        // SAFETY: MCHBAR remains enabled after raminit.
        let r = unsafe { &*(self.config.mchbar as *const PmRegs) };
        update16(&r.rffc, 0, 1 << 4);
    }

    pub(super) fn enable_iommu_final(&self) {
        // SAFETY: MCHBAR remains enabled after raminit.
        let r = unsafe { &*(self.config.mchbar as *const PmRegs) };
        update32(&r.r94, 0, 1 << 3);
    }
}

fn update8_byte(reg: &MmioReadWrite<u16>, clear: u8, set: u8) {
    update16(reg, u16::from(clear), u16::from(set));
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn gm45_pm_fsb_table_rejects_unknown_straps() {
        assert_eq!(fsb_values(667), Some((0x0ea6, 0x0e, 0x600, 0x1d80, 0x0b96)));
        assert_eq!(fsb_values(800).unwrap().0, 0x1194);
        assert_eq!(fsb_values(1067).unwrap().4, 0x10a4);
        assert_eq!(fsb_values(533), None);
    }
}
