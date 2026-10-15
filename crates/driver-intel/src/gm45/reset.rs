//! Cantiga reset preparation; an unprepared CF9 reset can power the board off.
//! Register sequence: coreboot gm45/early_reset.c and raminit.c.
use super::IntelGm45;
use fstart_core::mmio::MmioReadWrite;
use fstart_pci::ecam::EcamDevice;
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};
use tock_registers::{register_bitfields, register_structs};

register_bitfields![u32,
    Clock [UPDATE OFFSET(3) NUMBITS(1) [], POWER_UP OFFSET(21) NUMBITS(2) []],
    Dcc [COMMAND OFFSET(16) NUMBITS(3) [Nop = 1], INITIALIZED OFFSET(19) NUMBITS(1) []],
    Drc0 [RANKS OFFSET(24) NUMBITS(4) []],
    Drc1 [NOT_POPULATED OFFSET(16) NUMBITS(4) []],
    Drc2 [NOT_POPULATED OFFSET(24) NUMBITS(4) []]
];
register_structs! {
    Channel {
        (0x00 => boundaries: [MmioReadWrite<u32>; 2]),
        (0x08 => _reserved),
        (0x30 => drc0: MmioReadWrite<u32, Drc0::Register>),
        (0x34 => drc1: MmioReadWrite<u32, Drc1::Register>),
        (0x38 => drc2: MmioReadWrite<u32, Drc2::Register>),
        (0x3c => _tail),
        (0x100 => @END),
    },
    Registers {
        (0x000 => _reserved0),
        (0x200 => dcc: MmioReadWrite<u32, Dcc::Register>),
        (0x204 => _reserved1),
        (0xc00 => clock: MmioReadWrite<u32, Clock::Register>),
        (0xc04 => _reserved2),
        (0xc1c => scratch: MmioReadWrite<u16>),
        (0xc1e => _reserved3),
        (0xf14 => pm_status: MmioReadWrite<u32>),
        (0xf18 => _reserved4),
        (0x1200 => channels: [Channel; 2]),
        (0x1400 => @END),
    }
}

fn prepare(r: &Registers) {
    r.clock.modify(Clock::POWER_UP.val(0) + Clock::UPDATE::SET);
    for (index, ch) in r.channels.iter().enumerate() {
        ch.drc0.modify(Drc0::RANKS.val(u32::from(index == 0)));
        ch.drc1
            .modify(Drc1::NOT_POPULATED.val(if index == 0 { 14 } else { 15 }));
        ch.drc2
            .modify(Drc2::NOT_POPULATED.val(if index == 0 { 14 } else { 15 }));
        for boundary in &ch.boundaries {
            boundary.set(0x0004_0004);
        }
    }
    r.dcc.modify(Dcc::COMMAND::Nop);
}

impl IntelGm45 {
    fn reset_regs(&self) -> &Registers {
        // SAFETY: reset checks follow setup_early_decode, MCHBAR is enabled.
        unsafe { &*(self.config.mchbar as *const Registers) }
    }

    pub(super) fn prepare_reset_geometry(&self) {
        let r = self.reset_regs();
        prepare(r);
        let hb = self.hostbridge();
        hb.jedec_control.set(hb.jedec_control.get() & !4);
        hb.jedec_control.set(hb.jedec_control.get() | 4);
        r.dcc.modify(Dcc::INITIALIZED::SET);
    }

    fn cold_reset(&self) -> ! {
        self.prepare_reset_geometry();
        fstart_arch::x86::boot::system_reset(true)
    }

    /// Bootblock raminit finished and left its scratchpad marker, with no RAM
    /// initialization in progress. True in every later stage of the same boot.
    pub(super) fn raminit_completed(&self) -> bool {
        self.reset_regs().scratch.get() == 0xcafe
            && EcamDevice::new(0, 0x1f, 0).read8(0xa2) & (1 << 7) == 0
    }

    /// Coreboot romstage soft-reset and interrupted-raminit checks. Bootblock
    /// only: later stages see the same marker without GEN_RST_STS.
    pub(super) fn recover_early_reset(&self) {
        let lpc = EcamDevice::new(0, 0x1f, 0);
        let power = lpc.read16(0xa4);
        lpc.write16(0xa4, power); // acknowledge sticky W1C reset status
        if self.reset_regs().scratch.get() == 0xcafe && power & (1 << 9) == 0 {
            fstart_log::info!("gm45: completed warm boot needs a prepared reset");
            self.cold_reset();
        }
        let progress = lpc.read8(0xa2);
        if progress & (1 << 7) != 0 {
            lpc.write8(0xa2, progress & !(1 << 7));
            fstart_log::info!("gm45: interrupted RAM initialization, resetting");
            self.cold_reset();
        }
    }

    pub(super) fn enter_raminit(&self) {
        let lpc = EcamDevice::new(0, 0x1f, 0);
        lpc.or8(0xa2, 1 << 7);
        let r = self.reset_regs();
        let status = r.pm_status.get();
        r.pm_status.set(status); // W1C: not a generic modify
        if status & 2 != 0 && status & 1 == 0 {
            // Clear our in-progress flag before the deliberate reset.
            lpc.and8(0xa2, !(1 << 7));
            self.cold_reset();
        }
    }

    pub(super) fn finish_raminit(&self) {
        EcamDevice::new(0, 0x1f, 0).and8(0xa2, !(1 << 7));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reset_geometry_matches_coreboot_and_preserves_unrelated_bits() {
        let mut words = [u32::MAX; 0x1400 / 4];
        let regs = unsafe { &*(words.as_mut_ptr() as *const Registers) };
        prepare(regs);
        assert_eq!(regs.channels[0].drc0.read(Drc0::RANKS), 1);
        assert_eq!(regs.channels[1].drc0.read(Drc0::RANKS), 0);
        assert_eq!(regs.channels[0].drc1.read(Drc1::NOT_POPULATED), 14);
        assert_eq!(regs.channels[1].drc2.read(Drc2::NOT_POPULATED), 15);
        assert_eq!(regs.channels[0].boundaries[1].get(), 0x0004_0004);
        assert_eq!(regs.clock.read(Clock::POWER_UP), 0);
        assert!(regs.clock.is_set(Clock::UPDATE));
        assert_eq!(regs.dcc.read(Dcc::COMMAND), 1);
        assert_eq!(words[0x1210 / 4], u32::MAX);
    }
}
