//! Crestline PEG detection and link policy. ASPM remains OS-owned.

use fstart_core::mmio::MmioReadWrite;
use fstart_pci::{EcamDevice, PciType1Config};
use tock_registers::{
    interfaces::{ReadWriteable, Readable, Writeable},
    register_bitfields,
};

register_bitfields![u32,
    /// PEG PCI 0xb4, 32-bit Slot Capabilities.
    SlotCapabilities [
        POWER_LIMIT OFFSET(7) NUMBITS(8) [],
        NO_COMMAND_COMPLETED OFFSET(18) NUMBITS(1) [],
        SLOT_NUMBER OFFSET(19) NUMBITS(13) []
    ],
    /// PEG PCI 0xf0, 32-bit power policy.
    PowerF0 [
        BIT5 OFFSET(5) NUMBITS(1) [],
        MODE OFFSET(24) NUMBITS(4) [],
        BIT31 OFFSET(31) NUMBITS(1) []
    ],
    /// PEG PCI 0xfc, 32-bit power policy.
    PowerFC [
        BIT0 OFFSET(0) NUMBITS(1) [],
        BIT1 OFFSET(1) NUMBITS(1) [],
        BIT4 OFFSET(4) NUMBITS(1) [],
        MODE OFFSET(5) NUMBITS(3) [],
        BIT10 OFFSET(10) NUMBITS(1) [],
        BIT13 OFFSET(13) NUMBITS(1) [],
        BIT14 OFFSET(14) NUMBITS(1) []
    ]
];
register_bitfields![u16,
    /// PEG PCI 0xb2, 16-bit Link Status (PCIe capability at 0xa0).
    LinkStatus [
        WIDTH OFFSET(4) NUMBITS(6) [],
        TRAINING OFFSET(11) NUMBITS(1) []
    ],
    /// PEG PCI 0xba, 16-bit Slot Status.
    SlotStatus [ PRESENT OFFSET(6) NUMBITS(1) [] ],
    /// PEG PCI 0x224, 16-bit negotiated width control.
    Width [
        WIDTH OFFSET(0) NUMBITS(5) [],
        BIT8 OFFSET(8) NUMBITS(1) []
    ]
];

tock_registers::register_structs! {
    /// GM965 PEG configuration, including its PCIe capability.
    PegConfig {
        (0x00 => header: PciType1Config),
        (0x40 => _reserved0),
        (0xb2 => link_status: MmioReadWrite<u16, LinkStatus::Register>),
        (0xb4 => slot_capabilities: MmioReadWrite<u32, SlotCapabilities::Register>),
        (0xb8 => _reserved1),
        (0xba => slot_status: MmioReadWrite<u16, SlotStatus::Register>),
        (0xbc => _reserved2),
        (0xf0 => power_f0: MmioReadWrite<u32, PowerF0::Register>),
        (0xf4 => power_f4: MmioReadWrite<u32>),
        (0xf8 => _reserved3),
        (0xfc => power_fc: MmioReadWrite<u32, PowerFC::Register>),
        (0x100 => _reserved4),
        (0x104 => vc_cap: MmioReadWrite<u8>),
        (0x105 => _reserved5),
        (0x200 => power_200: MmioReadWrite<u32>),
        (0x204 => _reserved6),
        (0x224 => width: MmioReadWrite<u16, Width::Register>),
        (0x226 => @END),
    }
}

fn regs() -> &'static PegConfig {
    // SAFETY: caller enables DEVEN.D1F0 and ECAM before accessing PEG.
    unsafe { EcamDevice::new(0, 1, 0).regs::<PegConfig>() }
}

pub(super) fn has_display() -> bool {
    let peg = regs();
    if !peg.slot_status.is_set(SlotStatus::PRESENT) {
        return false;
    }
    for _ in 0..1000 {
        let status = peg.link_status.extract();
        if !status.is_set(LinkStatus::TRAINING) && status.read(LinkStatus::WIDTH) != 0 {
            // Borrow a bus only during discovery; preserve any previous assignment.
            let secondary = peg.header.secondary_bus.get();
            let subordinate = peg.header.subordinate_bus.get();
            peg.header.secondary_bus.set(1);
            peg.header.subordinate_bus.set(1);
            let gpu = EcamDevice::new(1, 0, 0);
            let found = gpu.is_present() && (gpu.read32(0x08) >> 24) == 3;
            peg.header.subordinate_bus.set(subordinate);
            peg.header.secondary_bus.set(secondary);
            return found;
        }
        fstart_arch::udelay(100);
    }
    false
}

pub(super) fn init() {
    configure(regs());
}

fn configure(peg: &PegConfig) {
    peg.width.modify(Width::WIDTH.val(17) + Width::BIT8::CLEAR);
    peg.slot_capabilities.write(
        SlotCapabilities::SLOT_NUMBER.val(1)
            + SlotCapabilities::NO_COMMAND_COMPLETED::SET
            + SlotCapabilities::POWER_LIMIT.val(75),
    );
    // Keep all traffic classes on VC0, matching the endpoint's default map.
    peg.vc_cap.set(peg.vc_cap.get() & !7);
    peg.power_200.set(peg.power_200.get() | (3 << 13));
    peg.power_f0
        .modify(PowerF0::BIT31::SET + PowerF0::MODE.val(4) + PowerF0::BIT5::SET);
    peg.power_f4.set(peg.power_f4.get() & !(1 << 4));
    peg.power_fc.modify(
        PowerFC::BIT0::SET
            + PowerFC::BIT1::CLEAR
            + PowerFC::BIT4::SET
            + PowerFC::MODE.val(1)
            + PowerFC::BIT10::CLEAR
            + PowerFC::BIT13::CLEAR
            + PowerFC::BIT14::CLEAR,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peg_policy_preserves_aspm_and_endpoint_traffic_classes() {
        let mut space = [0u32; 0x400];
        space[0x114 / 4] = 0x8000_00ff; // VC0: every traffic class.
        space[0xb0 / 4] = 0x1234_0000; // Link status/control; ASPM off.
        space[0xf0 / 4] = u32::MAX;
        space[0xfc / 4] = u32::MAX;
        // SAFETY: aligned memory models the complete PEG config window.
        let peg = unsafe { &*(space.as_mut_ptr() as *const PegConfig) };
        configure(peg);
        assert_eq!(
            peg.slot_capabilities.get(),
            (1 << 19) | (1 << 18) | (75 << 7)
        );
        assert_eq!(peg.width.get(), 17);
        assert_eq!(space[0x114 / 4], 0x8000_00ff);
        assert_eq!(space[0xb0 / 4], 0x1234_0000);
        assert_eq!(space[0xf0 / 4] & 0x8f00_0020, 0x8400_0020);
        assert_eq!(space[0xfc / 4] & 0x64f3, 0x31);
        for register in [0xa08, 0xa84, 0xb00, 0xb04, 0xb14] {
            assert_eq!(space[register / 4], 0);
        }
    }
}
