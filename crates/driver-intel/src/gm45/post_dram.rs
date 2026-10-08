//! GM45 romstage work following DDR3 training (coreboot gm45/pm.c,
//! pcie.c and iommu.c). ICH9 DMI setup follows training/PM and precedes this.

use super::{Gm45GraphicsPolicy, IntelGm45};
use fstart_core::mmio::MmioReadWrite;
use fstart_core::services::ServiceError;
use fstart_pci::ecam::EcamDevice;
use tock_registers::interfaces::{Readable, Writeable};
use tock_registers::register_structs;

register_structs! {
    DmiRegs {
        (0x000 => _pad0: [u8; 4]),
        (0x004 => port_cap: MmioReadWrite<u32>),
        (0x008 => _pad00: [u8; 0xc]),
        (0x014 => vc0: MmioReadWrite<u32>),
        (0x018 => _pad1: [u8; 4]),
        (0x01c => cap: MmioReadWrite<u32>),
        (0x020 => vc1: MmioReadWrite<u32>),
        (0x024 => _pad2: [u8; 2]),
        (0x026 => status: MmioReadWrite<u16>),
        (0x028 => _pad3: [u8; 4]),
        (0x02c => cir: MmioReadWrite<u32>),
        (0x030 => _pad4: [u8; 0x14]),
        (0x044 => esd: MmioReadWrite<u32>),
        (0x048 => _pad41: [u8; 8]),
        (0x050 => link1: MmioReadWrite<u32>),
        (0x054 => _pad42: [u8; 4]),
        (0x058 => link1_addr: MmioReadWrite<u32>),
        (0x05c => _pad43: [u8; 4]),
        (0x060 => link2: MmioReadWrite<u32>),
        (0x064 => _pad44: [u8; 4]),
        (0x068 => link2_addr: MmioReadWrite<u32>),
        (0x06c => _pad45: [u8; 0x18]),
        (0x084 => link_cap: MmioReadWrite<u32>),
        (0x088 => link_ctl: MmioReadWrite<u16>),
        (0x08a => _pad46: [u8; 0x66]),
        (0x0f0 => f0: MmioReadWrite<u32>),
        (0x0f4 => f4: MmioReadWrite<u32>),
        (0x0f8 => _pad5: [u8; 4]),
        (0x0fc => fc: MmioReadWrite<u32>),
        (0x100 => _pad6: [u8; 0x100]),
        (0x200 => ctl: MmioReadWrite<u32>),
        (0x204 => link: MmioReadWrite<u32>),
        (0x208 => cap2: MmioReadWrite<u32>),
        (0x20c => _pad7: [u8; 0xfc]),
        (0x308 => lock1: MmioReadWrite<u32>),
        (0x30c => _pad70: [u8; 8]),
        (0x314 => lock2: MmioReadWrite<u32>),
        (0x318 => _pad701: [u8; 0xc]),
        (0x324 => lock3: MmioReadWrite<u32>),
        (0x328 => lock4: MmioReadWrite<u32>),
        (0x32c => _pad702: [u8; 8]),
        (0x334 => lock5: MmioReadWrite<u32>),
        (0x338 => lock6: MmioReadWrite<u32>),
        (0x33c => _pad703: [u8; 0xae0]),
        (0xe1c => e1c: MmioReadWrite<u16>),
        (0xe1e => _pad71: [u8; 0x0e]),
        (0xe2c => e2c: MmioReadWrite<u32>),
        (0xe30 => _pad72: [u8; 0xd0]),
        (0xf00 => f00: MmioReadWrite<u16>),
        (0xf02 => _pad73: [u8; 2]),
        (0xf04 => timeout: MmioReadWrite<u32>),
        (0xf08 => _pad8: [u8; 0x0c]),
        (0xf14 => f14: MmioReadWrite<u32>),
        (0xf18 => _pad81: [u8; 0x2c]),
        (0xf44 => timeout2: MmioReadWrite<u32>),
        (0xf48 => _pad9: [u8; 0x3c]),
        (0xf84 => timeout3: MmioReadWrite<u32>),
        (0xf88 => _pad10: [u8; 0x3c]),
        (0xfc4 => timeout4: MmioReadWrite<u32>),
        (0xfc8 => @END),
    }
}
register_structs! {
    EgressRegs {
        (0x000 => _pad0: [u8; 4]),
        (0x004 => port_cap: MmioReadWrite<u32>),
        (0x008 => _pad00: [u8; 0xc]),
        (0x014 => vc0: MmioReadWrite<u32>),
        (0x018 => _pad1: [u8; 4]),
        (0x01c => cap: MmioReadWrite<u32>),
        (0x020 => vc1: MmioReadWrite<u32>),
        (0x024 => _pad2: [u8; 2]),
        (0x026 => status: MmioReadWrite<u16>),
        (0x028 => mts: MmioReadWrite<u32>),
        (0x02c => _pad3: [u8; 0x18]),
        (0x044 => esd: MmioReadWrite<u32>),
        (0x048 => _pad31: [u8; 8]),
        (0x050 => link1: MmioReadWrite<u32>),
        (0x054 => _pad32: [u8; 4]),
        (0x058 => link1_addr: MmioReadWrite<u32>),
        (0x05c => _pad33: [u8; 4]),
        (0x060 => link2: MmioReadWrite<u32>),
        (0x064 => _pad34: [u8; 0x9c]),
        (0x100 => pat: [MmioReadWrite<u32>; 8]),
        (0x120 => @END),
    }
}
register_structs! {
    IommuBars {
        (0x000 => _pad: [u8; 0x10]),
        (0x010 => me: MmioReadWrite<u32>),
        (0x014 => _pad1: [u8; 4]),
        (0x018 => gfx: MmioReadWrite<u32>),
        (0x01c => _pad2: [u8; 4]),
        (0x020 => vc0: MmioReadWrite<u32>),
        (0x024 => _pad3: [u8; 4]),
        (0x028 => vc1: MmioReadWrite<u32>),
        (0x02c => @END),
    }
}

fn wait_clear(reg: &MmioReadWrite<u16>, mask: u16) -> Result<(), ServiceError> {
    for _ in 0..0x7ffff {
        if reg.get() & mask == 0 {
            return Ok(());
        }
        core::hint::spin_loop();
    }
    Err(ServiceError::HardwareError)
}

impl IntelGm45 {
    pub(super) fn post_dram_pcie(&self, revision: u8) -> Result<(), ServiceError> {
        if self.graphics != Gm45GraphicsPolicy::Integrated {
            // PEG slot and power facts are not present in chipset config.
            return Err(ServiceError::InvalidParam);
        }
        // SAFETY: the enabled BARs are checked in new_from_config.
        let lpc_id = EcamDevice::new(0, 0x1f, 0).read16(2);
        let aspm = match lpc_id {
            0x2917 | 0x2919 => 3,
            0x2912 | 0x2914 | 0x2916 | 0x2918 => 1,
            _ => return Err(ServiceError::HardwareError),
        };
        let rcba_reg = EcamDevice::new(0, 0x1f, 0).read32(0xf0);
        let rcba = rcba_reg & 0xffff_c000;
        if rcba == 0 || rcba_reg & 1 == 0 {
            return Err(ServiceError::HardwareError);
        }
        let ep = unsafe { &*(self.config.epbar as *const EgressRegs) };
        let dmi = unsafe { &*(self.config.dmibar as *const DmiRegs) };
        // Coreboot clears only VC0's low byte [7:1]; upper control fields
        // must survive this setup.
        ep.vc0.set(ep.vc0.get() & !0xfe);
        ep.port_cap.set((ep.port_cap.get() & !7) | 1);
        ep.mts.set(0x0a0a_0a0a);
        ep.cap.set((ep.cap.get() & !(127 << 16)) | (10 << 16));
        ep.vc1
            .set((ep.vc1.get() & !((7 << 24) | 0xfe)) | (1 << 24) | 0x80);
        for (i, word) in ep.pat.iter().enumerate() {
            word.set(if i == 7 { 0x5555 } else { 0x5555_5555 });
        }
        ep.vc1.set(ep.vc1.get() | (1 << 16));
        wait_clear(&ep.status, 1)?;
        ep.vc1.set(ep.vc1.get() | (1 << 31));
        wait_clear(&ep.status, 2)?;

        dmi.vc0.set(dmi.vc0.get() & !0xfe);
        dmi.port_cap.set((dmi.port_cap.get() & !7) | 1);
        dmi.vc1
            .set((dmi.vc1.get() & !((7 << 24) | 0xfe)) | (1 << 24) | 0x80 | (1 << 31));
        wait_clear(&dmi.status, 2)?;
        dmi.ctl
            .set((dmi.ctl.get() | (3 << 13)) & !(1 << 21 | 3 << 26) | (2 << 26));
        dmi.cir.set(0x8600_0040);
        let fc = (dmi.fc.get() | 0x13) & !(1 << 11) | if revision < 6 { 1 << 11 } else { 0 };
        dmi.fc.set(fc);
        // DMILCTL2 de-emphasis was cleared before raminit; do not replay it.
        dmi.f4.set(dmi.f4.get() & !(1 << 4));
        dmi.f0.set(dmi.f0.get() | (3 << 24));
        for timeout in [&dmi.timeout, &dmi.timeout2, &dmi.timeout3, &dmi.timeout4] {
            timeout.set(0x0705_0880);
        }
        // Write-once DMI registers are latched after VC1 setup.
        for lock in [
            &dmi.lock1, &dmi.lock2, &dmi.lock3, &dmi.lock4, &dmi.lock5, &dmi.lock6,
        ] {
            lock.set(lock.get());
        }
        // The host and southbridge must advertise matching L0s/L1 support.
        dmi.link_ctl.set(dmi.link_ctl.get() | aspm);
        // Coreboot setup_aspm: DMI prerequisites, then write-once LCTL.
        dmi.link_cap
            .set((dmi.link_cap.get() & !(63 << 12)) | (2 << 12) | (2 << 15));
        dmi.cap2.set(dmi.cap2.get() & !(3 << 20 | 0xff00_0000));
        // DMI clock gating and L0s/L1 exit timing (gm45/pcie.c).
        dmi.f00.set(dmi.f00.get() | (3 << 8) | (7 << 3));
        dmi.f14.set(dmi.f14.get() & !(1 << 17));
        dmi.e1c.set((dmi.e1c.get() | 1) & !(1 << 8));
        dmi.e2c.set((dmi.e2c.get() & !(15 << 20)) | (13 << 20));
        dmi.e2c.set(0x88d0_7333);
        dmi.esd.set((dmi.esd.get() & !0x00ff_0000) | (1 << 16));
        dmi.link1.set((2 << 16) | 1);
        dmi.link1_addr.set(rcba);
        dmi.link2
            .set((dmi.link2.get() & !0x00ff_ffff) | (1 << 16) | 1);
        dmi.link2_addr.set(self.config.mchbar as u32);
        ep.esd.set((ep.esd.get() & !0x00ff_0000) | (1 << 16));
        ep.link1
            .set((ep.link1.get() & !0x00ff_ffff) | (1 << 16) | 1);
        ep.link1_addr.set(self.config.dmibar as u32);
        Ok(())
    }

    pub(super) fn post_dram_iommu(&self, revision: u8) {
        // SAFETY: MCHBAR is enabled and checked at construction.
        let bars = unsafe { &*(self.config.mchbar as *const IommuBars) };
        bars.vc1.set(0xfed9_0001);
        bars.gfx.set(if revision == 6 { 0 } else { 0xfed9_1001 });
        let me = EcamDevice::new(0, 3, 0);
        bars.me.set(if me.read8(0x08) != 0xff {
            0xfed9_2001
        } else {
            0
        });
        bars.vc0.set(0xfed9_3001);
        if revision == 7 {
            self.enable_b3_iommu_workaround();
            let peg = EcamDevice::new(0, 1, 0);
            if peg.read8(0x08) != 0xff {
                peg.or32(0xfc, 1 << 15);
            }
        }
        self.enable_iommu_final();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gm45_dmi_egress_and_remap_offsets_match_coreboot() {
        use core::mem::offset_of;
        assert_eq!(offset_of!(DmiRegs, port_cap), 0x04);
        assert_eq!(offset_of!(DmiRegs, link_ctl), 0x88);
        assert_eq!(offset_of!(DmiRegs, timeout4), 0xfc4);
        assert_eq!(offset_of!(EgressRegs, pat), 0x100);
        assert_eq!(offset_of!(EgressRegs, link1_addr), 0x58);
        assert_eq!(offset_of!(IommuBars, gfx), 0x18);
        assert_eq!(offset_of!(IommuBars, vc1), 0x28);
    }

    #[test]
    fn gm45_vc1_wait_is_bounded() {
        let mut memory = [0u32; 0x400];
        let regs = unsafe { &*(memory.as_mut_ptr() as *const DmiRegs) };
        assert!(wait_clear(&regs.status, 2).is_ok());
        regs.status.set(2);
        assert!(matches!(
            wait_clear(&regs.status, 2),
            Err(ServiceError::HardwareError)
        ));
    }
}
