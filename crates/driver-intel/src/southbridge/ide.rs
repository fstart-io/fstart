//! ICH PATA/IDE function (0:1f.1), shared by ICH7 and ICH8.
//!
//! Ported from coreboot `southbridge/intel/i82801{g,h}x/ide.c`.

use fstart_pci::ecam::EcamDevice;
use fstart_pci::{PCI_COMMAND_BITS, PciType0Config};
use tock_registers::interfaces::{ReadWriteable, Writeable};
use tock_registers::{LocalRegisterCopy, register_bitfields};

/// PATA/IDE controller configuration.
#[derive(Debug, Clone, Copy)]
pub struct IdeConfig {
    /// Enable the primary PATA channel.
    pub enable_primary: bool,
    /// Enable the secondary PATA channel.
    pub enable_secondary: bool,
}

/// PATA function location on the root bus.
pub const IDE_DEV: u8 = 0x1f;
pub const IDE_FUNC: u8 = 1;

const PROG_IF: u16 = 0x09;
const IDE_TIM_PRI: u16 = 0x40;
const IDE_TIM_SEC: u16 = 0x42;
const IDE_CONFIG: u16 = 0x54;

register_bitfields![u16,
    IDE_TIM [
        TIME0 OFFSET(0) NUMBITS(1) [],
        IE0 OFFSET(1) NUMBITS(1) [],
        RCT OFFSET(8) NUMBITS(2) [OneClock = 3],
        ISP OFFSET(12) NUMBITS(2) [ThreeClocks = 2],
        SITRE OFFSET(14) NUMBITS(1) [],
        DECODE_ENABLE OFFSET(15) NUMBITS(1) []
    ]
];

register_bitfields![u32,
    IDE_CONFIG_REG [
        PCB0 OFFSET(0) NUMBITS(1) [],
        PCB1 OFFSET(1) NUMBITS(1) [],
        SCB0 OFFSET(2) NUMBITS(1) [],
        SCB1 OFFSET(3) NUMBITS(1) [],
        FAST_PCB0 OFFSET(12) NUMBITS(1) [],
        FAST_PCB1 OFFSET(13) NUMBITS(1) [],
        FAST_SCB0 OFFSET(14) NUMBITS(1) [],
        FAST_SCB1 OFFSET(15) NUMBITS(1) []
    ]
];

/// Channel timing: decode enabled with coreboot's fixed PIO timings, or
/// decode disabled. Unrelated timing bits are preserved either way.
fn channel_timing(current: u16, enable: bool) -> u16 {
    let mut timing = LocalRegisterCopy::<u16, IDE_TIM::Register>::new(current);
    timing.modify(IDE_TIM::DECODE_ENABLE::CLEAR + IDE_TIM::SITRE::SET);
    if enable {
        timing.modify(
            IDE_TIM::DECODE_ENABLE::SET
                + IDE_TIM::ISP::ThreeClocks
                + IDE_TIM::RCT::OneClock
                + IDE_TIM::IE0::SET
                + IDE_TIM::TIME0::SET,
        );
    }
    timing.get()
}

fn io_config(config: &IdeConfig) -> u32 {
    let mut reg = LocalRegisterCopy::<u32, IDE_CONFIG_REG::Register>::new(0);
    if config.enable_primary {
        reg.modify(
            IDE_CONFIG_REG::FAST_PCB0::SET
                + IDE_CONFIG_REG::PCB0::SET
                + IDE_CONFIG_REG::FAST_PCB1::SET
                + IDE_CONFIG_REG::PCB1::SET,
        );
    }
    if config.enable_secondary {
        reg.modify(
            IDE_CONFIG_REG::FAST_SCB0::SET
                + IDE_CONFIG_REG::SCB0::SET
                + IDE_CONFIG_REG::FAST_SCB1::SET
                + IDE_CONFIG_REG::SCB1::SET,
        );
    }
    reg.get()
}

/// Program the PATA function (`ide_init`). Does nothing when it is hidden.
pub fn init(config: &IdeConfig) {
    let ide = EcamDevice::new(0, IDE_DEV, IDE_FUNC);
    if ide.read16(0) == 0xffff {
        return;
    }
    // SAFETY: the PATA function is present and has a Type 0 header.
    let regs = unsafe { ide.regs::<PciType0Config>() };
    regs.command
        .modify(PCI_COMMAND_BITS::IO_SPACE::SET + PCI_COMMAND_BITS::BUS_MASTER::SET);
    // Native capable, not enabled. Prog IF is read-only in the generic overlay.
    ide.write8(PROG_IF, 0x8a);
    ide.write16(
        IDE_TIM_PRI,
        channel_timing(ide.read16(IDE_TIM_PRI), config.enable_primary),
    );
    ide.write16(
        IDE_TIM_SEC,
        channel_timing(ide.read16(IDE_TIM_SEC), config.enable_secondary),
    );
    ide.write32(IDE_CONFIG, io_config(config));
    // The pin is assigned through DxxIP; legacy mode needs no line.
    regs.interrupt_line.set(0xff);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timings_match_coreboot_encoding() {
        assert_eq!(channel_timing(0x0000, true), 0xe303);
        assert_eq!(channel_timing(0x8000, false), 0x4000);
        let both = IdeConfig {
            enable_primary: true,
            enable_secondary: true,
        };
        assert_eq!(io_config(&both), 0xf00f);
    }
}
