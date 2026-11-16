// SPDX-License-Identifier: MIT OR Apache-2.0
//! The FCH ACPI-MMIO block (`FED8_0000h`–`FED8_1FFFh`), its banks and
//! registers, and the ACPI I/O blocks.
//!
//! Layout and names from PPR 55570 "ACPI MMIO Space Allocation" (the same
//! layout on Sandstone, Taishan and Huashan). The block is also reachable
//! over SMN at [`SMN_BASE`] on every die/socket (FCH_SP3 §4.2, Oxide
//! `amdzen/fch.h`), and can be relocated with PMxD5/D6 ([`relocate`]).
//! Registers are offsets in an FCH's [`FchHw::acpi`](super::FchHw::acpi)
//! window; [`PmIo`] reaches the PM bank through the `CD6h/CD7h` index pair
//! instead.

use embedded_hal::delay::DelayNs;
use fstart_core::indexed::Indexed;
use tock_registers::{
    LocalRegisterCopy,
    interfaces::{ReadWriteable, Readable, Writeable},
    register_bitfields,
};

use super::{PM_INDEX, indexed_io};
use crate::table::{self, Record};
use crate::window::{At, Ports, Window, indexed};

/// Host physical base of the primary FCH's block.
pub const BASE: u64 = super::ACPI_MMIO as u64;
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

/// The banks, each with `at`, a register of the bank by its offset (for
/// the vendor tables whose bits have no known meaning).
macro_rules! bank {
    ($(#[$meta:meta])* $name:ident, $base:expr) => {
        $(#[$meta])*
        pub mod $name {
            use crate::window::{At, Width};
            use tock_registers::RegisterLongName;
            pub const BASE: u32 = $base;
            pub const fn at<T: Width, R: RegisterLongName>(offset: u32) -> At<T, R> {
                At::new(BASE + offset)
            }
        }
    };
}
bank!(
    /// SMI and SCI control.
    smi_bank,
    super::SMI
);
bank!(
    /// Power management.
    pm,
    super::PM
);
bank!(
    /// The ACPI fixed registers (PM1 status/enable at 0/2).
    acpi,
    super::ACPI
);
bank!(
    /// The ASF controller.
    asf,
    super::ASF
);
bank!(
    /// The SMBus controller.
    smbus,
    super::SMBUS
);
bank!(
    /// One byte per pad: its function.
    iomux,
    super::IOMUX
);
bank!(
    /// Clock generator and miscellaneous control.
    misc,
    super::MISC
);
bank!(
    /// GPIO bank 0: the control register of pin n at 4 * n.
    gpio_bank,
    super::GPIO
);
bank!(
    /// Always-on device power control.
    aoac,
    super::AOAC
);

register_bitfields![u8,
    /// AOACx40 + 2n FCH::AOAC::DevD3Ctl.
    pub AoacControl [
        TARGET OFFSET(0) NUMBITS(2) [
            Initialized = 1
        ],
        DEVICE_STATE OFFSET(2) NUMBITS(1) [],
        /// PwrOnDev.
        POWER OFFSET(3) NUMBITS(1) [],
        SW_PWR_ON_RSTB OFFSET(4) NUMBITS(1) [],
        SW_REF_CLK_OK OFFSET(5) NUMBITS(1) [],
        SW_RSTB OFFSET(6) NUMBITS(1) [],
        IS_SW_CONTROL OFFSET(7) NUMBITS(1) []
    ],
    /// AOACx41 + 2n FCH::AOAC::DevD3State.
    pub AoacState [
        POWER_RESET OFFSET(0) NUMBITS(1) [],
        REFERENCE_CLOCK OFFSET(1) NUMBITS(1) []
    ],
    /// PMx54 FCH::PM::SerialIrqCfg.
    pub SerialIrq [
        NUM_START_BITS OFFSET(0) NUMBITS(2) [],
        NUM_SER_IRQ_BITS OFFSET(2) NUMBITS(4) [],
        QUIET_MODE OFFSET(6) NUMBITS(1) [],
        ENABLE OFFSET(7) NUMBITS(1) []
    ],
    /// PMx74 FCH::PM::AcpiConfig.
    pub AcpiConfig [
        DECODE_STD OFFSET(0) NUMBITS(1) [],
        GLOBAL_EN OFFSET(1) NUMBITS(1) [],
        RTC_EN_EN OFFSET(2) NUMBITS(1) [],
        TIMER_EN_EN OFFSET(4) NUMBITS(1) []
    ],
    /// PMx75: PMx74[15:8].
    pub AcpiConfigHigh [
        PWRBTN_EN OFFSET(0) NUMBITS(1) []
    ],
    /// PMxBE FCH::PM::ResetCtl1.
    pub ResetControl1 [
        KB_RESET_EN OFFSET(4) NUMBITS(1) [],
        /// SlpTypeControl: the FCH sleeps on the OS's SLP_TYP itself.
        SLP_TYPE_EN OFFSET(5) NUMBITS(1) []
    ],
    /// PMx5B FCH::PM::RTCShadow: the AC-loss policy. The setting goes into
    /// the low nibble, hardware copies it up.
    pub RtcShadow [
        POWER_FAILURE OFFSET(0) NUMBITS(2) [
            Off = 0,
            On = 1,
            Previous = 3
        ],
        /// PwrFailShadowEn; has to be written as 1.
        POWER_STATE OFFSET(2) NUMBITS(1) [],
        BIT3 OFFSET(3) NUMBITS(1) []
    ],
    /// PMxD3.
    pub SdControl [
        LEGACY_SD OFFSET(6) NUMBITS(1) []
    ],
    /// PMxD5 FCH::PM::AltAcpiMmioEn.
    pub AltMmioEn [
        ENABLE OFFSET(0) NUMBITS(1) [],
        WIDTH_64 OFFSET(1) NUMBITS(1) []
    ],
    /// PMxE8.
    pub SdEnable [
        ENABLE OFFSET(0) NUMBITS(1) []
    ],
    /// PMxEC.
    pub LpcGating [
        LPC_ENABLE OFFSET(0) NUMBITS(1) []
    ],
    /// PMx0A.
    pub Pm0a [
        BIT3 OFFSET(3) NUMBITS(1) [],
        BIT4 OFFSET(4) NUMBITS(1) []
    ],
    /// PMxC0 FCH::PM::S5ResetStat, its low byte (write-1-to-clear status).
    pub S5ResetStatus [
        THERMAL_TRIP OFFSET(0) NUMBITS(1) [],
        FOUR_SECOND_PWR_BTN OFFSET(1) NUMBITS(1) [],
        SHUTDOWN OFFSET(2) NUMBITS(1) []
    ]
];
register_bitfields![u32,
    /// PMx00 FCH::PM::PmDecodeEn.
    pub Decode [
        LEGACY OFFSET(0) NUMBITS(1) [],
        CF9 OFFSET(1) NUMBITS(1) [],
        DMA OFFSET(2) NUMBITS(1) [],
        DMA80 OFFSET(3) NUMBITS(1) [],
        SMBUS_ASF_IO OFFSET(4) NUMBITS(1) [],
        IOAPIC OFFSET(5) NUMBITS(1) [],
        HPET OFFSET(6) NUMBITS(1) [],
        WATCHDOG OFFSET(7) NUMBITS(1) [],
        SMBUS_ASF_IO_BASE OFFSET(8) NUMBITS(8) [],
        ASF_SM_MASTER OFFSET(16) NUMBITS(1) [],
        ASF_CLK_STRETCH OFFSET(17) NUMBITS(1) [],
        ASF_CLK_SWITCH OFFSET(18) NUMBITS(1) [],
        SMBUS0_SEL OFFSET(19) NUMBITS(2) [],
        HPET64 OFFSET(28) NUMBITS(1) [],
        HPET_MSI OFFSET(29) NUMBITS(1) []
    ],
    /// PMx04 FCH::PM::IsaControl.
    pub IsaControl [
        BIOS_RAM_EN OFFSET(0) NUMBITS(1) [],
        MMIO_EN OFFSET(1) NUMBITS(1) [],
        AB_CLK_GATE_EN OFFSET(16) NUMBITS(1) []
    ],
    /// PMx08.
    pub PciControl [
        FORCE_STPCLK_RETRY OFFSET(24) NUMBITS(1) [],
        FORCE_SLPSTATE_RETRY OFFSET(25) NUMBITS(1) []
    ],
    /// PMxE0: the A-Link index/data I/O base.
    pub AbRegBar [
        PORT OFFSET(0) NUMBITS(16) []
    ],
    /// MISCx08: CGPLLConfig1.
    pub CgPllConfig1 [
        CG1_SPREAD_SPECTRUM OFFSET(0) NUMBITS(1) []
    ],
    /// MISCx10: CGPLLConfig3.
    pub CgPllConfig3 [
        CG1_FRACN_EN_OVERRIDE OFFSET(29) NUMBITS(1) []
    ],
    /// MISCx40.
    pub MiscClkCntl1 [
        ALINK_GATE OFFSET(1) NUMBITS(1) [],
        CG1_FBDIV_LOADEN OFFSET(25) NUMBITS(1) [],
        /// Applies the CG1 PLL settings; self-clearing.
        CG1_CFG_UPDATE_REQ OFFSET(30) NUMBITS(1) []
    ],
    /// AOACx94: bits 15:14 are cleared at reset and set at Sx entry
    /// (FCH_SP3 §3.1 af4f, §5 Sx entry; FchTaishanSmmInit). Not named in
    /// the public PPRs.
    pub Aoac94 [
        SX OFFSET(14) NUMBITS(2) []
    ]
];

pub const PM_DECODE: At<u32, Decode::Register> = pm::at(0x00);
pub const PM_ISA_CONTROL: At<u32, IsaControl::Register> = pm::at(0x04);
pub const PM_PCI_CONTROL: At<u32, PciControl::Register> = pm::at(0x08);
pub const PM_0A: At<u8, Pm0a::Register> = pm::at(0x0a);
pub const PM_0B: At<u8> = pm::at(0x0b);
pub const PM_SERIAL_IRQ: At<u8, SerialIrq::Register> = pm::at(0x54);
pub const PM_RTC_SHADOW: At<u8, RtcShadow::Register> = pm::at(0x5b);
pub const PM_RTC_EXT_INDEX: At<u8> = pm::at(0x5e);
pub const PM_RTC_EXT_DATA: At<u8> = pm::at(0x5f);
/// The ACPI I/O blocks (PMx60..PMx6E).
pub const PM_ACPI_PM1_EVT: At<u16> = pm::at(0x60);
pub const PM_ACPI_PM1_CNT: At<u16> = pm::at(0x62);
pub const PM_ACPI_PM_TMR: At<u16> = pm::at(0x64);
pub const PM_ACPI_CPU_CNT: At<u16> = pm::at(0x66);
pub const PM_ACPI_GPE0: At<u16> = pm::at(0x68);
pub const PM_ACPI_SMI_CMD: At<u16> = pm::at(0x6a);
pub const PM_ACPI_PMA_CNT: At<u16> = pm::at(0x6c);
pub const PM_ACPI_PM2_CNT: At<u16> = pm::at(0x6e);
pub const PM_ACPI_CONFIG: At<u8, AcpiConfig::Register> = pm::at(0x74);
pub const PM_ACPI_CONFIG_HIGH: At<u8, AcpiConfigHigh::Register> = pm::at(0x75);
pub const PM_RESET_CONTROL1: At<u8, ResetControl1::Register> = pm::at(0xbe);
pub const PM_S5_RESET_STATUS: At<u8, S5ResetStatus::Register> = pm::at(0xc0);
/// PMxC8 FCH::PM::Misc.
pub const PM_MISC: At<u8> = pm::at(0xc8);
pub const PM_D2: At<u8> = pm::at(0xd2);
/// PMxD3 (FCH::PM::ManualReset in the SP3 reports): the legacy SD decode.
pub const PM_SD_CONTROL: At<u8, SdControl::Register> = pm::at(0xd3);
pub const PM_ALT_MMIO_EN: At<u8, AltMmioEn::Register> = pm::at(0xd5);
/// PMxD6/D7: the relocated block's base\[31:16\].
pub const PM_ALT_MMIO_BASE: At<u16> = pm::at(0xd6);
pub const PM_AB_REG_BAR: At<u32, AbRegBar::Register> = pm::at(0xe0);
pub const PM_SD_ENABLE: At<u8, SdEnable::Register> = pm::at(0xe8);
pub const PM_LPC_GATING: At<u8, LpcGating::Register> = pm::at(0xec);
pub const PM_EE: At<u8> = pm::at(0xee);
pub const MISC_CGPLL_CONFIG1: At<u32, CgPllConfig1::Register> = misc::at(0x08);
pub const MISC_CGPLL_CONFIG3: At<u32, CgPllConfig3::Register> = misc::at(0x10);
pub const MISC_CLK_CNTL1: At<u32, MiscClkCntl1::Register> = misc::at(0x40);
/// The PM1 block through the ACPI MMIO.
pub const ACPI_PM1_STATUS: At<u16, Pm1Status::Register> = acpi::at(0x00);
pub const ACPI_PM1_ENABLE: At<u16, Pm1Enable::Register> = acpi::at(0x02);
pub const AOAC_94: At<u32, Aoac94::Register> = aoac::at(0x94);
/// The AOAC D3 control and state of a device.
pub const fn aoac_control(device: u8) -> At<u8, AoacControl::Register> {
    aoac::at(0x40 + 2 * device as u32)
}
pub const fn aoac_state(device: u8) -> At<u8, AoacState::Register> {
    aoac::at(0x41 + 2 * device as u32)
}

// ---- The ACPI I/O blocks ----

register_bitfields![u8,
    /// PM1_STS\[15:8\], as the vendor code reads and clears it.
    pub Pm1StatusHigh [
        PWRBTN OFFSET(0) NUMBITS(1) [],
        RTC OFFSET(2) NUMBITS(1) [],
        PCIEXP_WAKE OFFSET(6) NUMBITS(1) []
    ]
];

register_bitfields![u16,
    /// PM1_STS.
    pub Pm1Status [
        PWRBTN OFFSET(8) NUMBITS(1) [],
        PCIEXP_WAKE OFFSET(14) NUMBITS(1) [],
        WAK OFFSET(15) NUMBITS(1) []
    ],
    /// PM1_EN.
    pub Pm1Enable [
        PWRBTN OFFSET(8) NUMBITS(1) [],
        PCIEXP_WAKE_DIS OFFSET(14) NUMBITS(1) []
    ],
    /// PM1_CNT.
    pub Pm1Control [
        SCI_EN OFFSET(0) NUMBITS(1) [],
        SLP_TYP OFFSET(10) NUMBITS(3) [
            S5 = 5
        ],
        SLP_EN OFFSET(13) NUMBITS(1) []
    ]
];

/// The ACPI fixed I/O blocks an FCH decodes (PMx60..PMx68).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AcpiPorts {
    pub event: u16,
    pub control: u16,
    pub timer: u16,
    pub gpe: u16,
}

/// The ACPI fixed I/O blocks as the SP3 ResetHwAcpi programs them
/// (PMx60..6E, FCH_SP3 §3.1 af40; same on all three generations).
pub const PM1_STS: u32 = 0x800;
pub const PM1_EN: u32 = 0x802;
pub const PM1_CNT: u32 = 0x804;
pub const PM_TMR: u32 = 0x808;
pub const CPU_CNT: u32 = 0x810;
pub const GPE0_STS: u32 = 0x820;
pub const GPE0_EN: u32 = 0x824;
pub const PM2_CNT: u32 = 0xBA;

/// The PM block through the `CD6h/CD7h` byte index pair, as a window with
/// ACPI-MMIO offsets (`PM` + n). Needed before PMx04[MmioEn] is set and on a
/// remote FCH while the DF routes legacy I/O to it ([`super::multi`]).
pub struct PmIo<I>(pub I);

impl<I: Window> PmIo<I> {
    fn port(&self, off: u32) -> Indexed<crate::window::Reg<&I, u8>, crate::window::Reg<&I, u8>> {
        debug_assert!((PM..PM + 0x100).contains(&off));
        indexed(
            &self.0,
            PM_INDEX.into(),
            u32::from(PM_INDEX) + 1,
            (off - PM) as u8,
        )
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

/// AOAC devices with evidence in the vendor code (FCH_SP3 §3.1 af44, §3.2
/// af55, coreboot's Picasso). The public PPRs do not list AOAC device
/// numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AoacDevice(pub u8);

impl AoacDevice {
    /// Legacy SD/eMMC host (FCH_SP3 §3.2 af55: `fed81e72` bit 3).
    pub const SD: Self = Self(0x19);
    /// eSPI (FCH_SP3 §3.1 af44: `fed81e76` bit 3).
    pub const ESPI: Self = Self(0x1B);
}

/// Power a device on (`PwrOnDev` = 1) or off.
pub fn aoac_power<W: Window>(acpi: &W, dev: AoacDevice, on: bool) {
    acpi.at(aoac_control(dev.0))
        .modify(AoacControl::POWER.val(on.into()));
}

/// Whether ACPI-MMIO decode is on (PMx04[MmioEn]).
pub fn mmio_enabled<W: Window>(pm: &W) -> bool {
    pm.at(PM_ISA_CONTROL).is_set(IsaControl::MMIO_EN)
}

/// Relocate an FCH's block to `base` (8 KiB aligned, below 4 GiB) through
/// its PM registers: PMxD6/D7 = base\[31:16\], PMxD5 32-bit + enable
/// (FCH_SP3 §4.1).
pub fn relocate<W: Window>(pm: &W, base: u32) {
    debug_assert_eq!(base % SIZE, 0);
    pm.at(PM_ALT_MMIO_BASE).set((base >> 16) as u16);
    pm.at(PM_ALT_MMIO_EN)
        .modify(AltMmioEn::WIDTH_64::CLEAR + AltMmioEn::ENABLE::SET);
}

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    InvalidDevice,
    PowerTimeout(u8),
    Unavailable(u8),
}
fn valid_device(device: u8) -> bool {
    matches!(device, 0 | 7..=9 | 11..=12 | 16..=17 | 26..=28)
}

/// Apply a vendor ACPI MMIO table ([`crate::table`]) in order.
pub fn apply<T: crate::window::Width>(
    acpi: &impl Window,
    records: impl IntoIterator<Item = Record<At<T>, T>>,
) {
    table::apply(records, |reg| acpi.at(reg));
}

/// Validate all devices/controls before power writes. Start every device
/// before waiting; all-ones is unavailable, not a proof that power/clock
/// are ready. After any programming error the caller must stop/reset, not
/// retry. Polling is bounded, `polls` times 100 us.
pub fn power_devices(
    acpi: &impl Window,
    devices: &[u8],
    polls: u32,
    delay: &mut impl DelayNs,
) -> Result<(), Error> {
    if devices.iter().any(|&id| !valid_device(id)) {
        return Err(Error::InvalidDevice);
    }
    let mut controls = [0; 29];
    for &id in devices {
        let control = acpi.at(aoac_control(id)).get();
        if control == u8::MAX {
            return Err(Error::Unavailable(id));
        }
        controls[usize::from(id)] = control;
    }
    for &id in devices {
        let mut control =
            LocalRegisterCopy::<u8, AoacControl::Register>::new(controls[usize::from(id)]);
        control.modify(AoacControl::POWER::SET + AoacControl::TARGET::Initialized);
        acpi.at(aoac_control(id)).set(control.get());
    }
    for &id in devices {
        let mut remaining = polls;
        loop {
            let state = acpi.at(aoac_state(id)).extract();
            if state.get() == u8::MAX {
                return Err(Error::Unavailable(id));
            }
            if state.matches_all(AoacState::POWER_RESET::SET + AoacState::REFERENCE_CLOCK::SET) {
                break;
            }
            if remaining == 0 {
                return Err(Error::PowerTimeout(id));
            }
            delay.delay_us(100);
            remaining -= 1;
        }
    }
    Ok(())
}
pub fn enable_legacy_decode(acpi: &impl Window) {
    acpi.at(PM_DECODE)
        .modify(Decode::LEGACY::SET + Decode::CF9::SET);
}
pub fn disable_legacy_dma(acpi: &impl Window) {
    acpi.at(PM_DECODE)
        .modify(Decode::DMA::CLEAR + Decode::DMA80::CLEAR);
}
pub fn enable_hpet_ioapic(acpi: &impl Window) {
    acpi.at(PM_DECODE).modify(
        Decode::IOAPIC::SET + Decode::HPET::SET + Decode::HPET_MSI::SET + Decode::HPET64::CLEAR,
    );
}
/// fch_init_acpi_decode: the ACPI blocks and their standard decode.
pub fn acpi_decode(acpi: &impl Window, ports: AcpiPorts) {
    acpi.at(PM_ACPI_PM1_EVT).set(ports.event);
    acpi.at(PM_ACPI_PM1_CNT).set(ports.control);
    acpi.at(PM_ACPI_PM_TMR).set(ports.timer);
    acpi.at(PM_ACPI_GPE0).set(ports.gpe);
    acpi.at(PM_ACPI_CONFIG).modify(AcpiConfig::DECODE_STD::SET);
}
/// fch_init_acpi_ports: the ACPI blocks and the standard features. The SMI
/// command port belongs to [`super::smi::enable`].
pub fn acpi_ports(acpi: &impl Window, ports: AcpiPorts) {
    acpi_decode(acpi, ports);
    acpi.at(PM_ACPI_CONFIG).write(
        AcpiConfig::DECODE_STD::SET
            + AcpiConfig::GLOBAL_EN::SET
            + AcpiConfig::RTC_EN_EN::SET
            + AcpiConfig::TIMER_EN_EN::SET,
    );
}
pub fn disable_keyboard_reset(acpi: &impl Window) {
    acpi.at(PM_RESET_CONTROL1)
        .modify(ResetControl1::KB_RESET_EN::CLEAR);
}
/// The legacy SD controller off: its decode and its AOAC power.
pub fn legacy_sd_off(acpi: &impl Window) {
    acpi.at(PM_SD_CONTROL).modify(SdControl::LEGACY_SD::CLEAR);
    acpi.at(PM_SD_ENABLE).modify(SdEnable::ENABLE::CLEAR);
    acpi.at(aoac_control(AoacDevice::SD.0))
        .modify(AoacControl::POWER::CLEAR);
}

#[cfg(target_arch = "x86_64")]
/// Enable the decode using indexed BYTE I/O. PMx04 is read/written as FOUR
/// separate bytes, preserving adjacent BIOSRAM/other decode policy.
/// # Safety
/// Ring 0, exclusive access to CD6/CD7. MMIO remains UC during and after this.
pub unsafe fn enable_acpimmio() {
    // SAFETY: the caller owns CD6/CD7; the ACPI MMIO isn't decoded yet.
    let ports = unsafe { Ports::new() };
    let byte = |i: u8| indexed_io::<_, ()>(&ports, PM_INDEX, 4 + i);
    let mut bytes = [0u8; 4];
    for (i, value) in bytes.iter_mut().enumerate() {
        *value = byte(i as u8).get();
    }
    bytes[0] |= 2;
    for (i, value) in bytes.into_iter().enumerate() {
        byte(i as u8).set(value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fch::model::Mem;

    #[test]
    fn pm_through_index_ports() {
        let io = Mem::default();
        let pm = PmIo(io.clone());
        pm.write16(PM_ALT_MMIO_BASE.offset, 0xec30);
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
        pm.put(PM_ALT_MMIO_EN.offset, 1, 0x02);
        relocate(&pm, 0xec30_0000);
        assert_eq!(pm.get(PM + 0xd6, 2), 0xec30);
        assert_eq!(pm.get(PM + 0xd5, 1), 0x01);
    }
}
