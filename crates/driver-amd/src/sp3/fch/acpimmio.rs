//! The FCH ACPI-MMIO block (`FED8_0000h`–`FED8_1FFFh`) and the ACPI I/O
//! blocks.
//!
//! Layout and names from PPR 55570 "ACPI MMIO Space Allocation" (the same
//! layout on Taishan and Huashan, FCH_SP3 §3 shorthand). The block is also
//! reachable over SMN at [`SMN_BASE`] on every die/socket (FCH_SP3 §4.2,
//! Oxide `amdzen/fch.h`), and can be relocated with PMxD5/D6.

use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};
use tock_registers::register_bitfields;

use super::Window;
use fstart_core::indexed::Indexed;

/// Host physical base of the primary FCH's block.
pub const BASE: u64 = 0xFED8_0000;
/// The same block over SMN (`+ offset`), per die (Naples) or socket.
pub const SMN_BASE: u32 = 0x02D0_1000;
/// Size of the block (and of an ALTMMIO relocation).
pub const SIZE: u32 = 0x2000;

/// Block offsets.
pub const SMBUS_PCI: u32 = 0x000;
pub const SMI: u32 = 0x200;
pub const PM: u32 = 0x300;
pub const PM2: u32 = 0x400;
pub const BIOS_RAM: u32 = 0x500;
pub const CMOS_RAM: u32 = 0x600;
pub const CMOS: u32 = 0x700;
pub const ACPI: u32 = 0x800;
pub const ASF: u32 = 0x900;
pub const SMBUS: u32 = 0xA00;
pub const WDT: u32 = 0xB00;
pub const HPET: u32 = 0xC00;
pub const IOMUX: u32 = 0xD00;
pub const MISC: u32 = 0xE00;
pub const REMOTE_GPIO: u32 = 0x1200;
pub const GPIO: u32 = 0x1500;
pub const WAKE: u32 = 0x1D00;
pub const AOAC: u32 = 0x1E00;

/// PM register offsets (PMxNN, relative to [`PM`]).
pub mod pm {
    pub const DECODE_EN: u32 = 0x00;
    pub const ISA_CONTROL: u32 = 0x04;
    pub const PCI_CONTROL: u32 = 0x08;
    pub const PM_0A: u32 = 0x0A;
    pub const PM_0B: u32 = 0x0B;
    pub const SERIAL_IRQ: u32 = 0x54;
    pub const RTC_SHADOW: u32 = 0x5B;
    pub const RTC_EXT_INDEX: u32 = 0x5E;
    pub const RTC_EXT_DATA: u32 = 0x5F;
    pub const ACPI_PM1_EVT: u32 = 0x60;
    pub const ACPI_PM1_CNT: u32 = 0x62;
    pub const ACPI_PM_TMR: u32 = 0x64;
    pub const ACPI_CPU_CNT: u32 = 0x66;
    pub const ACPI_GPE0: u32 = 0x68;
    pub const ACPI_SMI_CMD: u32 = 0x6A;
    pub const ACPI_PM2_CNT: u32 = 0x6E;
    pub const ACPI_CONFIG: u32 = 0x74;
    pub const RESET_CONTROL1: u32 = 0xBE;
    pub const S5_RESET_STATUS: u32 = 0xC0;
    pub const MISC: u32 = 0xC8;
    pub const PM_D2: u32 = 0xD2;
    pub const MANUAL_RESET: u32 = 0xD3;
    pub const ALT_MMIO_EN: u32 = 0xD5;
    pub const ALT_MMIO_BASE: u32 = 0xD6;
    pub const AB_REG_BAR: u32 = 0xE0;
    pub const PM_E8: u32 = 0xE8;
    pub const PM_EE: u32 = 0xEE;
}

register_bitfields![u32,
    /// PMx00 FCH::PM::PmDecodeEn.
    pub DECODE_EN [
        LEGACY_IO OFFSET(0) NUMBITS(1) [],
        CF9_IO OFFSET(1) NUMBITS(1) [],
        LEGACY_DMA_IO OFFSET(2) NUMBITS(1) [],
        DMA_PORT80 OFFSET(3) NUMBITS(1) [],
        SMBUS_ASF_IO_EN OFFSET(4) NUMBITS(1) [],
        IOAPIC_EN OFFSET(5) NUMBITS(1) [],
        HPET_EN OFFSET(6) NUMBITS(1) [],
        WATCHDOG_EN OFFSET(7) NUMBITS(1) [],
        SMBUS_ASF_IO_BASE OFFSET(8) NUMBITS(8) [],
        ASF_SM_MASTER_EN OFFSET(16) NUMBITS(1) [],
        ASF_CLK_STRETCH_EN OFFSET(17) NUMBITS(1) [],
        ASF_CLK_SWITCH OFFSET(18) NUMBITS(1) [],
        SMBUS0_SEL OFFSET(19) NUMBITS(2) [],
        HPET_WIDTH_SEL OFFSET(28) NUMBITS(1) [],
        HPET_MSI_EN OFFSET(29) NUMBITS(1) []
    ],
    /// PMx04 FCH::PM::IsaControl.
    pub ISA_CONTROL [
        BIOS_RAM_EN OFFSET(0) NUMBITS(1) [],
        MMIO_EN OFFSET(1) NUMBITS(1) [],
        AB_CLK_GATE_EN OFFSET(16) NUMBITS(1) []
    ],
    /// PMxC0 FCH::PM::S5ResetStat (write-1-to-clear status).
    pub S5_RESET_STATUS [
        THERMAL_TRIP OFFSET(0) NUMBITS(1) [],
        FOUR_SECOND_PWR_BTN OFFSET(1) NUMBITS(1) [],
        SHUTDOWN OFFSET(2) NUMBITS(1) [],
        USER_RST OFFSET(16) NUMBITS(1) [],
        SLEEP_RESET OFFSET(21) NUMBITS(1) [],
        SYNC_FLOOD OFFSET(27) NUMBITS(1) []
    ]
];

register_bitfields![u8,
    /// PMx54 FCH::PM::SerialIrqCfg.
    pub SERIAL_IRQ [
        NUM_START_BITS OFFSET(0) NUMBITS(2) [],
        NUM_SER_IRQ_BITS OFFSET(2) NUMBITS(4) [],
        QUIET_MODE OFFSET(6) NUMBITS(1) [],
        ENABLE OFFSET(7) NUMBITS(1) []
    ],
    /// PMx5B FCH::PM::RTCShadow: the AC-loss policy (FCH_SP3 §8).
    pub RTC_SHADOW [
        PWR_FAIL OFFSET(0) NUMBITS(2) [Off = 0, On = 1, Previous = 3],
        PWR_FAIL_SHADOW_EN OFFSET(2) NUMBITS(1) [],
        PWR_FAIL_BIT3 OFFSET(3) NUMBITS(1) []
    ],
    /// PMxBE FCH::PM::ResetCtl1.
    pub RESET_CONTROL1 [
        SLP_TYPE_CONTROL OFFSET(5) NUMBITS(1) []
    ],
    /// PMxD5 FCH::PM::AltAcpiMmioEn.
    pub ALT_MMIO_EN [
        ENABLE OFFSET(0) NUMBITS(1) [],
        WIDTH_64 OFFSET(1) NUMBITS(1) []
    ],
    /// AOACx40 + 2n FCH::AOAC::DevD3Ctl.
    pub AOAC_CTL [
        TARGET_STATE OFFSET(0) NUMBITS(2) [],
        DEVICE_STATE OFFSET(2) NUMBITS(1) [],
        PWR_ON_DEV OFFSET(3) NUMBITS(1) [],
        SW_PWR_ON_RSTB OFFSET(4) NUMBITS(1) [],
        SW_REF_CLK_OK OFFSET(5) NUMBITS(1) [],
        SW_RSTB OFFSET(6) NUMBITS(1) [],
        IS_SW_CONTROL OFFSET(7) NUMBITS(1) []
    ]
];

register_bitfields![u16,
    /// PM1_CNT (ACPI I/O, [`PM1_CNT`]).
    pub PM1_CNT [
        SCI_EN OFFSET(0) NUMBITS(1) [],
        SLP_TYP OFFSET(10) NUMBITS(3) [],
        SLP_EN OFFSET(13) NUMBITS(1) []
    ],
    /// PM1_STS (ACPI I/O, [`PM1_STS`]).
    pub PM1_STS [
        PWRBTN_STS OFFSET(8) NUMBITS(1) [],
        WAK_STS OFFSET(15) NUMBITS(1) []
    ]
];

/// The ACPI fixed I/O blocks as programmed by ResetHwAcpi (PMx60..6E,
/// FCH_SP3 §3.1 af40; same on all three generations).
pub const PM1_STS: u32 = 0x800;
pub const PM1_EN: u32 = 0x802;
pub const PM1_CNT: u32 = 0x804;
pub const PM_TMR: u32 = 0x808;
pub const CPU_CNT: u32 = 0x810;
pub const GPE0_STS: u32 = 0x820;
pub const GPE0_EN: u32 = 0x824;
pub const SMI_CMD: u32 = 0xB2;
pub const PM2_CNT: u32 = 0xBA;

/// PM index/data ports (PMIO before ACPI-MMIO decode is on).
pub const PM_INDEX: u32 = 0xCD6;
pub const PM_DATA: u32 = 0xCD7;

/// The PM block through the `CD6h/CD7h` byte index pair, as a window with
/// ACPI-MMIO offsets (`PM` + n). Needed before PMx04[MmioEn] is set and on a
/// remote FCH while the DF routes legacy I/O to it ([`super::multi`]).
pub struct PmIo<I>(pub I);

impl<I: Window> PmIo<I> {
    fn port(&self, off: u32) -> Indexed<super::Reg<'_, I, u8>, super::Reg<'_, I, u8>> {
        debug_assert!((PM..PM + 0x100).contains(&off));
        Indexed::new(self.0.reg(PM_INDEX), self.0.reg(PM_DATA), (off - PM) as u8)
    }
}

impl<I: Window> Window for PmIo<I> {
    fn read32(&self, off: u32) -> u32 {
        (0..4).fold(0, |v, i| v | u32::from(self.read8(off + i)) << (8 * i))
    }
    fn write32(&self, off: u32, value: u32) {
        (0..4).for_each(|i| self.write8(off + i, (value >> (8 * i)) as u8));
    }
    fn read16(&self, off: u32) -> u16 {
        u16::from(self.read8(off)) | u16::from(self.read8(off + 1)) << 8
    }
    fn write16(&self, off: u32, value: u16) {
        self.write8(off, value as u8);
        self.write8(off + 1, (value >> 8) as u8);
    }
    fn read8(&self, off: u32) -> u8 {
        self.port(off).get()
    }
    fn write8(&self, off: u32, value: u8) {
        self.port(off).set(value)
    }
}

/// AOAC devices with evidence in the donors (FCH_SP3 §3.1 af44, §3.2 af55).
/// The public PPRs do not list AOAC device numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AoacDevice(pub u8);

impl AoacDevice {
    /// Legacy SD/eMMC host (FCH_SP3 §3.2 af55: `fed81e72` bit 3).
    pub const SD: Self = Self(0x19);
    /// eSPI (FCH_SP3 §3.1 af44: `fed81e76` bit 3).
    pub const ESPI: Self = Self(0x1B);

    fn ctl(self) -> u32 {
        AOAC + 0x40 + 2 * u32::from(self.0)
    }
}

/// AOACx94: bits 15:14 are cleared at reset and set at Sx entry (FCH_SP3
/// §3.1 af4f, §5 Sx entry). Not named in the public PPRs.
pub const AOAC_94: u32 = AOAC + 0x94;
pub const AOAC_94_SX: u32 = 0xC000;

/// Power a device on (`PwrOnDev` = 1) or off.
pub fn aoac_power<W: Window>(acpi: &W, dev: AoacDevice, on: bool) {
    acpi.reg::<u8, AOAC_CTL::Register>(dev.ctl())
        .modify(AOAC_CTL::PWR_ON_DEV.val(on.into()));
}

/// Whether ACPI-MMIO decode is on (PMx04[MmioEn]).
pub fn mmio_enabled<W: Window>(pm: &W) -> bool {
    pm.reg::<u32, ISA_CONTROL::Register>(PM + pm::ISA_CONTROL)
        .is_set(ISA_CONTROL::MMIO_EN)
}

/// Relocate an FCH's block to `base` (8 KiB aligned, below 4 GiB) through
/// its PM registers: PMxD6/D7 = base\[31:16\], PMxD5 32-bit + enable
/// (FCH_SP3 §4.1).
pub fn relocate<W: Window>(pm: &W, base: u32) {
    debug_assert_eq!(base % SIZE, 0);
    pm.reg::<u16, ()>(PM + pm::ALT_MMIO_BASE)
        .set((base >> 16) as u16);
    pm.reg::<u8, ALT_MMIO_EN::Register>(PM + pm::ALT_MMIO_EN)
        .modify(ALT_MMIO_EN::WIDTH_64::CLEAR + ALT_MMIO_EN::ENABLE::SET);
}

#[cfg(test)]
mod tests {
    use super::super::model::Mem;
    use super::*;

    #[test]
    fn pm_through_index_ports() {
        let io = Mem::default();
        let pm = PmIo(io.clone());
        pm.write16(PM + pm::ALT_MMIO_BASE, 0xec30);
        assert_eq!(
            io.writes(),
            vec![
                (0xcd6, 1, 0xd6),
                (0xcd7, 1, 0x30),
                (0xcd6, 1, 0xd7),
                (0xcd7, 1, 0xec)
            ]
        );
    }

    #[test]
    fn relocation_matches_the_donor() {
        let pm = Mem::default();
        pm.put(PM + pm::ALT_MMIO_EN, 1, 0x02);
        relocate(&pm, 0xec30_0000);
        assert_eq!(pm.get(PM + 0xd6, 2), 0xec30);
        assert_eq!(pm.get(PM + 0xd5, 1), 0x01);
    }
}
