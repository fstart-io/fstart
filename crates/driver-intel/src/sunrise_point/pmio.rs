// SPDX-License-Identifier: MIT OR Apache-2.0
//! Sunrise Point ACPI I/O geometry from soc/intel/skylake/include/soc/pm.h.
//! GPE status/enable are four dwords at 80h/90h; neither ICH GPE layout nor
//! PMBASE+60h TCO is applicable here.

use crate::southbridge::pmio_ich::{PM_CONTROL, PM_STATUS};
use fstart_core::{pio::PioRegister, pio_register_structs};
use tock_registers::register_bitfields;

register_bitfields![u16,
    pub Enable [
        TIMER OFFSET(0) NUMBITS(1) [],
        GLOBAL OFFSET(5) NUMBITS(1) [],
        POWER_BUTTON OFFSET(8) NUMBITS(1) [],
        RTC OFFSET(10) NUMBITS(1) [],
        PCIEXP_WAKE_DISABLE OFFSET(14) NUMBITS(1) []
    ]
];
register_bitfields![u32,
    pub SmiEnable [
        GLOBAL OFFSET(0) NUMBITS(1) [],
        EOS OFFSET(1) NUMBITS(1) [],
        BIOS OFFSET(2) NUMBITS(1) [],
        LEGACY_USB OFFSET(3) NUMBITS(1) [],
        SLEEP OFFSET(4) NUMBITS(1) [],
        APM OFFSET(5) NUMBITS(1) [],
        SOFTWARE_TIMER OFFSET(6) NUMBITS(1) [],
        BIOS_RELEASE OFFSET(7) NUMBITS(1) [],
        MCSMI OFFSET(11) NUMBITS(1) [],
        TCO OFFSET(13) NUMBITS(1) [],
        PERIODIC OFFSET(14) NUMBITS(1) [],
        LEGACY_USB2 OFFSET(17) NUMBITS(1) [],
        INTEL_USB2 OFFSET(18) NUMBITS(1) [],
        GPIO_UNLOCK OFFSET(27) NUMBITS(1) [],
        ESPI OFFSET(28) NUMBITS(1) [],
        ME OFFSET(30) NUMBITS(1) [],
        XHCI OFFSET(31) NUMBITS(1) []
    ],
    pub SmiStatus [
        BIOS OFFSET(2) NUMBITS(1) [],
        LEGACY_USB OFFSET(3) NUMBITS(1) [],
        SLEEP OFFSET(4) NUMBITS(1) [],
        APM OFFSET(5) NUMBITS(1) [],
        SOFTWARE_TIMER OFFSET(6) NUMBITS(1) [],
        PM1 OFFSET(8) NUMBITS(1) [],
        GPE0 OFFSET(9) NUMBITS(1) [],
        GPIO OFFSET(10) NUMBITS(1) [],
        MCSMI OFFSET(11) NUMBITS(1) [],
        DEVMON OFFSET(12) NUMBITS(1) [],
        TCO OFFSET(13) NUMBITS(1) [],
        PERIODIC OFFSET(14) NUMBITS(1) [],
        SERIRQ OFFSET(15) NUMBITS(1) [],
        SMBUS OFFSET(16) NUMBITS(1) [],
        PCI_EXP OFFSET(20) NUMBITS(1) [],
        MONITOR OFFSET(21) NUMBITS(1) [],
        SCC OFFSET(25) NUMBITS(1) [],
        SPI OFFSET(26) NUMBITS(1) [],
        GPIO_UNLOCK OFFSET(27) NUMBITS(1) [],
        ESPI OFFSET(28) NUMBITS(1) [],
        SERIAL_IO OFFSET(29) NUMBITS(1) [],
        ME OFFSET(30) NUMBITS(1) [],
        XHCI OFFSET(31) NUMBITS(1) []
    ],
    pub UsbPower [
        SMI OFFSET(0) NUMBITS(1) [],
        WAKE OFFSET(1) NUMBITS(1) [],
        STATUS OFFSET(8) NUMBITS(1) []
    ],
    pub StandardGpe [
        HOTPLUG OFFSET(1) NUMBITS(1) [],
        SOFTWARE OFFSET(2) NUMBITS(1) [],
        TCO OFFSET(6) NUMBITS(1) [],
        SMBUS_WAKE OFFSET(7) NUMBITS(1) [],
        PCI_EXP OFFSET(9) NUMBITS(1) [],
        BATTERY_LOW OFFSET(10) NUMBITS(1) [],
        PME OFFSET(11) NUMBITS(1) [],
        ME OFFSET(12) NUMBITS(1) [],
        PME_B0 OFFSET(13) NUMBITS(1) [],
        ESPI OFFSET(14) NUMBITS(1) [],
        GPIO_TIER2 OFFSET(15) NUMBITS(1) [],
        LAN_WAKE OFFSET(16) NUMBITS(1) [],
        WADT OFFSET(18) NUMBITS(1) []
    ]
];

pio_register_structs! {
    pub(crate) AcpiIo {
        (0x00 => pub status: PioRegister<u16, PM_STATUS::Register>),
        (0x02 => pub enable: PioRegister<u16, Enable::Register>),
        (0x04 => pub control: PioRegister<u32, PM_CONTROL::Register>),
        (0x08 => pub timer: PioRegister<u32>),
        (0x30 => pub smi_enable: PioRegister<u32, SmiEnable::Register>),
        (0x34 => pub smi_status: PioRegister<u32, SmiStatus::Register>),
        (0x3c => pub usb_power: PioRegister<u32, UsbPower::Register>),
        (0x80 => pub gpe_status0: PioRegister<u32>),
        (0x84 => pub gpe_status1: PioRegister<u32>),
        (0x88 => pub gpe_status2: PioRegister<u32>),
        (0x8c => pub gpe_status3: PioRegister<u32>),
        (0x90 => pub gpe_enable0: PioRegister<u32>),
        (0x94 => pub gpe_enable1: PioRegister<u32>),
        (0x98 => pub gpe_enable2: PioRegister<u32>),
        (0x9c => pub gpe_enable3: PioRegister<u32>),
    }
}
impl AcpiIo {
    pub fn gpe_status(&self) -> [PioRegister<u32>; 4] {
        [
            self.gpe_status0(),
            self.gpe_status1(),
            self.gpe_status2(),
            self.gpe_status3(),
        ]
    }
    pub fn gpe_enable(&self) -> [PioRegister<u32>; 4] {
        [
            self.gpe_enable0(),
            self.gpe_enable1(),
            self.gpe_enable2(),
            self.gpe_enable3(),
        ]
    }
}
