// SPDX-License-Identifier: MIT OR Apache-2.0
//! PMC decodes, native DRAM reset/boot classification and GPIO GPE routing.
//! Derived from Skylake, native/reut.c, boot_mode.c and common pmc_gpe_init.
//! PWRMBASE is bootblock-owned;
//! do not try to rediscover it through a PMC function that may be hidden.

use super::{
    dmi,
    gpio::{self, Group, MiscConfig},
    pcr::{self, Pcr, Port},
};
use fstart_core::mmio::MmioReadWrite;
use fstart_pci::ecam::EcamDevice;
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};
use tock_registers::{LocalRegisterCopy, register_bitfields, register_structs};

pub const PWRM_BASE: usize = 0xfe00_0000;
pub const ACPI_BASE: u16 = 0x1800;
// D31:F2 ABASE (0x40, 32-bit) reads STYPE=1 for the I/O BAR.
const ABASE_READBACK: u32 = ACPI_BASE as u32 | 1;

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AfterPowerLoss {
    Off = 0,
    On = 1,
    Last = 2,
}

register_bitfields![u32,
    pub(crate) AcpiControl [
        SCI_SELECT OFFSET(0) NUMBITS(3) [],
        ACPI_ENABLE OFFSET(7) NUMBITS(1) [],
        PWRM_ENABLE OFFSET(8) NUMBITS(1) []
    ],
    pub(crate) GenPmconB [
        SLEEP_AFTER_POWER_FAIL OFFSET(0) NUMBITS(1) [],
        POWER_FAIL OFFSET(1) NUMBITS(1) [],
        RTC_BATTERY_DEAD OFFSET(2) NUMBITS(1) [],
        S4_ASSERT_ENABLE OFFSET(3) NUMBITS(1) [],
        S4_ASSERT_WIDTH OFFSET(4) NUMBITS(2) [],
        HOST_RESET OFFSET(9) NUMBITS(1) [],
        S3_ASSERT_WIDTH OFFSET(10) NUMBITS(2) [],
        DISABLE_SUS_STRETCH OFFSET(12) NUMBITS(1) [],
        WOL_OVERRIDE OFFSET(13) NUMBITS(1) [],
        SUSPEND_POWER_FAIL OFFSET(14) NUMBITS(1) [],
        PME_S5_DISABLE OFFSET(15) NUMBITS(1) [],
        DATA_BAR_DISABLE OFFSET(16) NUMBITS(1) [],
        ACPI_BASE_LOCK OFFSET(17) NUMBITS(1) [],
        STRETCH_LOCK OFFSET(18) NUMBITS(1) []
    ],
    pub(crate) PowerConfig [
        ENERGY_REPORT OFFSET(2) NUMBITS(1) [],
        POWER_CYCLE OFFSET(8) NUMBITS(2) [],
        SLP_A_ASSERT OFFSET(16) NUMBITS(2) [],
        SLP_SUS_ASSERT OFFSET(18) NUMBITS(2) [],
        ENERGY_REPORT2 OFFSET(24) NUMBITS(1) [],
        PMC_LOCK OFFSET(27) NUMBITS(1) [],
        OSC_SHUTDOWN OFFSET(29) NUMBITS(1) []
    ],
    pub(crate) PowerConfig3 [
        LOCK OFFSET(2) NUMBITS(1) [],
        WLAN_DEEP OFFSET(16) NUMBITS(1) [],
        WLAN_HOST OFFSET(17) NUMBITS(1) []
    ],
    pub(crate) PowerConfig4 [
        RAMP_DURATION OFFSET(0) NUMBITS(9) [],
        USB2_PHY_GATE OFFSET(30) NUMBITS(1) []
    ],
    pub(crate) DeepS34 [
        AC OFFSET(0) NUMBITS(1) [],
        DC OFFSET(1) NUMBITS(1) []
    ],
    pub(crate) DeepS5 [
        AC OFFSET(14) NUMBITS(1) [],
        DC OFFSET(15) NUMBITS(1) []
    ],
    pub(crate) DeepConfig [
        LAN_WAKE OFFSET(0) NUMBITS(1) [],
        DISABLE_AC_PULLDOWN OFFSET(1) NUMBITS(1) [],
        WAKE_PIN OFFSET(2) NUMBITS(1) []
    ],
    pub(crate) GatingPolicy [
        HYSTERESIS OFFSET(0) NUMBITS(9) [],
        MPG OFFSET(29) NUMBITS(1) [],
        ENABLE OFFSET(30) NUMBITS(1) []
    ],
    pub(crate) BmControl [
        SYNC_MSG OFFSET(9) NUMBITS(1) [],
        ZERO_ENABLE OFFSET(10) NUMBITS(1) [],
        HOLD_BLOCK OFFSET(14) NUMBITS(1) []
    ],
    pub(crate) ExtendedReset [
        CF9_WITHOUT_RESUME_WELL_RESET OFFSET(18) NUMBITS(1) [],
        GLOBAL_RESET OFFSET(20) NUMBITS(1) [],
        GLOBAL_RESET_LOCK OFFSET(24) NUMBITS(1) [],
        CF9_LOCK OFFSET(31) NUMBITS(1) []
    ],
    pub(crate) PcieMessageStatus [ FULL OFFSET(24) NUMBITS(1) [] ],
    pub(crate) PcieMessage [
        OPCODE OFFSET(0) NUMBITS(16) [],
        LANES OFFSET(16) NUMBITS(16) []
    ],
    pub(crate) FunctionDisable [
        GBE OFFSET(0) NUMBITS(1) [],
        ISH OFFSET(5) NUMBITS(1) [],
        CAM OFFSET(6) NUMBITS(1) [],
        LOCK OFFSET(31) NUMBITS(1) []
    ],
    pub(crate) SubsystemFuse [
        GBE OFFSET(0) NUMBITS(1) [],
        ISH OFFSET(1) NUMBITS(1) [],
        CAM OFFSET(2) NUMBITS(1) []
    ],
    pub(crate) SerialDisable [ FUNCTIONS OFFSET(0) NUMBITS(11) [] ],
    pub(crate) CppmControl [
        CIO2_DISABLE OFFSET(12) NUMBITS(1) [],
        SCS_DISABLE OFFSET(14) NUMBITS(1) []
    ],
    pub(crate) FinalLock [
        TT_STATE OFFSET(24) NUMBITS(2) [],
        TT_EN OFFSET(26) NUMBITS(1) [],
        LOCK OFFSET(31) NUMBITS(1) []
    ],
    pub(crate) NonStaticDisable [
        PCIE OFFSET(2) NUMBITS(20) [],
        SATA OFFSET(22) NUMBITS(1) [],
        ADSP OFFSET(23) NUMBITS(1) [],
        XDCI OFFSET(24) NUMBITS(1) [],
        SCC OFFSET(25) NUMBITS(1) []
    ]
];
register_bitfields![u32,
    pub Config2 [
        BUTTON_BEHAVIOR OFFSET(22) NUMBITS(2) [],
        DRAM_RESET_DEASSERT OFFSET(26) NUMBITS(1) [],
        BUTTON_DISABLE OFFSET(28) NUMBITS(1) [],
        BUTTON_OVERRIDE OFFSET(29) NUMBITS(3) []
    ],
    pub GenPmconA [
        SMI_LOCK OFFSET(4) NUMBITS(1) [],
        L1_BCLKREQ OFFSET(5) NUMBITS(1) [],
        L1_OPI OFFSET(6) NUMBITS(1) [],
        L1_C0 OFFSET(7) NUMBITS(1) [],
        SPXB_CLOCK_GATE OFFSET(12) NUMBITS(1) [],
        OPI_PLL_SHUTDOWN OFFSET(13) NUMBITS(1) [],
        GLOBAL_RESET_STATUS OFFSET(16) NUMBITS(1) [],
        MS4V OFFSET(18) NUMBITS(1) [],
        MEM_SR OFFSET(21) NUMBITS(1) [],
        DISB OFFSET(23) NUMBITS(1) [],
        LAN_SX OFFSET(27) NUMBITS(1) [],
        LAN_AG3 OFFSET(28) NUMBITS(1) [],
        LAN_DEEP_DISABLE OFFSET(29) NUMBITS(1) [],
        LAN_DC_DISABLE OFFSET(30) NUMBITS(1) []
    ],
    pub GpeConfig [
        DW0 OFFSET(0) NUMBITS(4) [],
        DW1 OFFSET(4) NUMBITS(4) [],
        DW2 OFFSET(8) NUMBITS(4) []
    ]
];
register_structs! {
    pub PmcRegisters {
        (0x00 => _reserved0),
        (0x10 => pub power_reset_status: fstart_core::mmio::MmioWriteOnly<u32>),
        (0x14 => pub(crate) power_management: MmioReadWrite<u32>),
        (0x18 => pub(crate) power_config: MmioReadWrite<u32, PowerConfig::Register>),
        (0x1c => pub(crate) pcie_message_status: fstart_core::mmio::MmioReadOnly<u32, PcieMessageStatus::Register>),
        (0x20 => pub(crate) pcie_message: MmioReadWrite<u32, PcieMessage::Register>),
        (0x24 => _power_reserved),
        (0x28 => pub(crate) deep_s3: MmioReadWrite<u32, DeepS34::Register>),
        (0x2c => pub(crate) deep_s4: MmioReadWrite<u32, DeepS34::Register>),
        (0x30 => pub(crate) deep_s5: MmioReadWrite<u32, DeepS5::Register>),
        (0x34 => pub(crate) deep_config: MmioReadWrite<u32, DeepConfig::Register>),
        (0x38 => _reserved_policy),
        (0x3c => pub config2:MmioReadWrite<u32,Config2::Register>),
        (0x40 => _reserved_ring),
        (0x48 => pub(crate) non_snoop: [MmioReadWrite<u32>; 5]),
        (0x5c => _reserved_non_snoop),
        (0x68 => pub(crate) non_snoop_sa: MmioReadWrite<u32>),
        (0x6c => _reserved_clock),
        (0x80 => pub(crate) clock_wake: [MmioReadWrite<u32>; 4]),
        (0x90 => _reserved_clock_cf),
        (0x98 => pub(crate) clock_wake_cf: MmioReadWrite<u32>),
        (0x9c => _reserved_power_ack),
        (0xa8 => pub(crate) power_ack: [MmioReadWrite<u32>; 4]),
        (0xb8 => _reserved_misc),
        (0xc0 => pub(crate) misc_event: MmioReadWrite<u32>),
        (0xc4 => pub(crate) pmsync: MmioReadWrite<u32, FinalLock::Register>),
        (0xc8 => _reserved_sync),
        (0xd0 => pub(crate) sync_state_hysteresis: MmioReadWrite<u32>),
        (0xd4 => pub(crate) sync_mode: MmioReadWrite<u32>),
        (0xd8 => _reserved_config3),
        (0xe0 => pub(crate) power_config3: MmioReadWrite<u32, PowerConfig3::Register>),
        (0xe4 => pub(crate) down_ppb: MmioReadWrite<u32>),
        (0xe8 => pub(crate) power_config4: MmioReadWrite<u32, PowerConfig4::Register>),
        (0xec => _reserved_sync_c0),
        (0xf4 => pub(crate) sync_c0: MmioReadWrite<u32>),
        (0xf8 => _reserved_vr),
        (0x100 => pub(crate) vr_misc: MmioReadWrite<u32>),
        (0x104 => _reserved1),
        (0x120 => pub gpio_gpe_config:MmioReadWrite<u32,GpeConfig::Register>),
        (0x124 => pub global_reset_cause: [MmioReadWrite<u32>; 2]),
        (0x12c => _reserved_modphy),
        (0x200 => pub(crate) modphy_config: [MmioReadWrite<u32>; 2]),
        (0x208 => _reserved_obff),
        (0x314 => pub(crate) obff: MmioReadWrite<u32>),
        (0x318 => _reserved_cppm),
        (0x31c => pub(crate) cppm_control: MmioReadWrite<u32, CppmControl::Register>),
        (0x320 => pub(crate) cppm_misc: MmioReadWrite<u32>),
        (0x324 => pub(crate) clock_gate1: MmioReadWrite<u32, GatingPolicy::Register>),
        (0x328 => _reserved_gate2),
        (0x340 => pub(crate) clock_gate2: MmioReadWrite<u32, GatingPolicy::Register>),
        (0x344 => _reserved_gate_misc),
        (0x34c => pub(crate) gate_misc: MmioReadWrite<u32>),
        (0x350 => _reserved_gate3),
        (0x3a8 => pub(crate) clock_gate3: MmioReadWrite<u32, GatingPolicy::Register>),
        (0x3ac => _reserved_mphy),
        (0x3d0 => pub(crate) mphy_policy: MmioReadWrite<u32>),
        (0x3d4 => _reserved_mpg),
        (0x3e0 => pub(crate) multipower_gate: MmioReadWrite<u32, GatingPolicy::Register>),
        (0x3e4 => _reserved_cs),
        (0x3e8 => pub(crate) clock_shutdown: [MmioReadWrite<u32>; 2]),
        (0x3f0 => _reserved2),
        (0x5d0 => pub hsw_power_gate: MmioReadWrite<u32>),
        (0x5d4 => _hsw_power_gate_reserved),
        (0x600 => pub(crate) serial_clock: [MmioReadWrite<u32>; 2]),
        (0x608 => _reserved_function_disable),
        (0x620 => pub(crate) function_disable: MmioReadWrite<u32, FunctionDisable::Register>),
        (0x624 => pub(crate) serial_disable: MmioReadWrite<u32, SerialDisable::Register>),
        (0x628 => pub(crate) non_static_disable: MmioReadWrite<u32, NonStaticDisable::Register>),
        (0x62c => _reserved_fuses),
        (0x644 => pub(crate) subsystem_fuse: fstart_core::mmio::MmioReadOnly<u32, SubsystemFuse::Register>),
        (0x648 => _reserved_end),
        (0x10000 => @END),
    }
}
impl PmcRegisters {
    /// # Safety
    /// `base` is a mapped and enabled 64 KiB PWRM window, exclusively owned
    /// for `'a`. This constructor does not enable the BAR/decode.
    pub unsafe fn at<'a>(base: usize) -> &'a Self {
        assert_eq!(base & 0xffff, 0);
        unsafe { &*(base as *const Self) }
    }
    /// Route the three GPIO GPE dwords consistently in PMC and all four
    /// PCH-H communities (native pmc_gpe_init/gpio_route_gpe). Caller owns a
    /// quiescent pre-OS event-routing phase; this neither acknowledges wake
    /// status nor enables ACPI GPE delivery. Board/ASL policy selects groups.
    ///
    /// Duplicate groups reject before writes, instead of the source's silent
    /// fallback to reset selectors. All PCR endpoints are validated first.
    pub fn configure_gpe_routes(
        &self,
        pcr: &Pcr,
        kind: dmi::Kind,
        groups: [Group; 3],
    ) -> Result<(), gpio::Error> {
        if groups[0] == groups[1] || groups[1] == groups[2] || groups[0] == groups[2] {
            return Err(gpio::Error::DuplicateGpeGroup);
        }
        let selectors = [
            u32::from(
                groups[0]
                    .info_for(kind)
                    .ok_or(gpio::Error::InvalidPad)?
                    .pmc_selector,
            ),
            u32::from(
                groups[1]
                    .info_for(kind)
                    .ok_or(gpio::Error::InvalidPad)?
                    .pmc_selector,
            ),
            u32::from(
                groups[2]
                    .info_for(kind)
                    .ok_or(gpio::Error::InvalidPad)?
                    .pmc_selector,
            ),
        ];
        let registers = [
            pcr.register::<MiscConfig::Register>(Port::Gpio0, 0x10)?,
            pcr.register::<MiscConfig::Register>(Port::Gpio1, 0x10)?,
            pcr.register::<MiscConfig::Register>(Port::Gpio2, 0x10)?,
            pcr.register::<MiscConfig::Register>(Port::Gpio3, 0x10)?,
        ];
        self.gpio_gpe_config.modify(
            GpeConfig::DW0.val(selectors[0])
                + GpeConfig::DW1.val(selectors[1])
                + GpeConfig::DW2.val(selectors[2]),
        );
        for register in registers {
            register.modify(
                MiscConfig::GPE0_DW0.val(selectors[0])
                    + MiscConfig::GPE0_DW1.val(selectors[1])
                    + MiscConfig::GPE0_DW2.val(selectors[2]),
            );
        }
        Ok(())
    }

    /// Bare RESET# toggle. CKE overrides, waits and MRS belong to raminit.
    pub fn assert_dram_reset(&self, assert: bool) {
        self.config2
            .modify(Config2::DRAM_RESET_DEASSERT.val(!assert as u32));
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    WrongFunction,
    Unavailable,
    Locked,
    Readback,
    Pcr(pcr::Error),
}
impl From<pcr::Error> for DecodeError {
    fn from(value: pcr::Error) -> Self {
        Self::Pcr(value)
    }
}

fn require_device(device: &EcamDevice) -> Result<(), DecodeError> {
    if device.address().segment() != 0
        || device.bus() != 0
        || device.dev() != 0x1f
        || device.func() != 2
    {
        return Err(DecodeError::WrongFunction);
    }
    if device.try_read16(0) != Some(0x8086) {
        return Err(DecodeError::Unavailable);
    }
    Ok(())
}

/// Verify the fixed PMC decodes before reading the ACPI I/O or PWRM windows.
/// Also used from standalone SMM; no logging or panicking ECAM accesses.
pub(crate) fn require_decodes(device: &EcamDevice) -> Result<(), DecodeError> {
    require_device(device)?;
    let actl = device.try_read32(0x44).ok_or(DecodeError::Readback)?;
    if device.try_read32(0x40) != Some(ABASE_READBACK)
        || device.try_read32(0x48) != Some(PWRM_BASE as u32)
        || !AcpiControl::ACPI_ENABLE.is_set(actl)
        || !AcpiControl::PWRM_ENABLE.is_set(actl)
    {
        return Err(DecodeError::Readback);
    }
    Ok(())
}

/// Native bootblock/pch.c ACPI/PWRM decode setup, PCH-H and PCH-LP. The caller
/// reserves these fixed resources and enables P2SB first. No wake W1C ack.
pub fn enable_decodes(device: &EcamDevice, pcr: &Pcr, kind: dmi::Kind) -> Result<(), DecodeError> {
    require_device(device)?;
    let status_b = unsafe { device.register::<u32, GenPmconB::Register>(0xa4) };
    if status_b.is_set(GenPmconB::ACPI_BASE_LOCK) {
        return Err(DecodeError::Locked);
    }
    let pmc_dest = match kind {
        dmi::Kind::H => 0x23a8u32,
        dmi::Kind::Lp => 0x23a0u32,
    };
    let control = unsafe { device.register::<u32, AcpiControl::Register>(0x44) };
    let original = control.get();
    control.set(original & !AcpiControl::ACPI_ENABLE::SET.value);
    unsafe { device.register::<u32, ()>(0x40).set(u32::from(ACPI_BASE)) };
    control.set(original | AcpiControl::ACPI_ENABLE::SET.value);
    pcr.register::<()>(Port::Dmi, 0x27b4)?
        .set((0x3f << 18) | u32::from(ACPI_BASE) | 1);
    pcr.register::<()>(Port::Dmi, 0x27b8)?.set(pmc_dest);

    let original = control.get();
    control.set(original & !AcpiControl::PWRM_ENABLE::SET.value);
    unsafe { device.register::<u32, ()>(0x48).set(PWRM_BASE as u32) };
    control.set(original | AcpiControl::PWRM_ENABLE::SET.value);
    pcr.register::<()>(Port::Dmi, 0x27ac)?
        .set((PWRM_BASE as u32 & 0xffff_0000) | (PWRM_BASE as u32 >> 16));
    pcr.register::<()>(Port::Dmi, 0x27b0)?
        .set(0x8000_0000 | pmc_dest);
    Ok(())
}

/// Match native pmc_clear_disb() after cold classification, before training.
/// Do not acknowledge the reset status bits that share this register.
pub(crate) fn clear_dram_initialized(device: &EcamDevice) -> Result<(), DecodeError> {
    require_device(device)?;
    let control = unsafe { device.register::<u32, GenPmconA::Register>(0xa0) };
    control.modify(
        GenPmconA::DISB::CLEAR + GenPmconA::GLOBAL_RESET_STATUS::CLEAR + GenPmconA::MS4V::CLEAR,
    );
    if control.is_set(GenPmconA::DISB) {
        return Err(DecodeError::Readback);
    }
    Ok(())
}

/// Select what the next CF9 reset does, before the reset the CSE asked for
/// in its DRAM Init Done reply (native romstage_native.c). CF9GR and CWORWRE
/// live in the resume well and may be left over from an earlier boot: clear
/// both, then set CF9GR for a global reset.
pub(crate) fn select_cf9_reset(device: &EcamDevice, global: bool) -> Result<(), DecodeError> {
    require_device(device)?;
    let reset = unsafe { device.register::<u32, ExtendedReset::Register>(0xac) };
    reset.modify(
        ExtendedReset::CF9_WITHOUT_RESUME_WELL_RESET::CLEAR
            + ExtendedReset::GLOBAL_RESET.val(u32::from(global)),
    );
    if reset.is_set(ExtendedReset::GLOBAL_RESET) != global {
        return Err(DecodeError::Readback);
    }
    Ok(())
}

/// Native pmutil.c DISB publication, called only after the actual controller,
/// CPU memory check and CSE continuation. Never acknowledge reset W1C bits.
pub(crate) fn mark_dram_initialized(device: &EcamDevice) -> Result<(), DecodeError> {
    require_device(device)?;
    let control = unsafe { device.register::<u32, GenPmconA::Register>(0xa0) };
    control.modify(
        GenPmconA::DISB::SET + GenPmconA::GLOBAL_RESET_STATUS::CLEAR + GenPmconA::MS4V::CLEAR,
    );
    if !control.is_set(GenPmconA::DISB) {
        return Err(DecodeError::Readback);
    }
    Ok(())
}

/// Non-destructive reset snapshot; capture before status acknowledgement.
/// PM1/GPE I/O wake state is captured separately by the platform, not inferred
/// from this subset (in particular it cannot identify S3 by itself).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResetState {
    pub gen_pmcon_a: u32,
    pub gen_pmcon_b: u32,
    pub global_reset_cause: [u32; 2],
}
impl PmcRegisters {
    pub fn capture_reset_state(&self, device: &EcamDevice) -> Result<ResetState, DecodeError> {
        require_device(device)?;
        Ok(ResetState {
            gen_pmcon_a: unsafe { device.register::<u32, GenPmconA::Register>(0xa0) }.get(),
            gen_pmcon_b: unsafe { device.register::<u32, GenPmconB::Register>(0xa4) }.get(),
            global_reset_cause: [
                self.global_reset_cause[0].get(),
                self.global_reset_cause[1].get(),
            ],
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryBootMode {
    Cold,
    Warm,
    S3,
}
impl MemoryBootMode {
    /// Interpret the *saved* PMC/PM1 snapshot. Never reread W1C status here.
    /// This classification is not permission to cold-train on warm/S3 boots.
    pub fn from_power_state(saved_gen_pmcon_a: u32, s3_wake: bool) -> Self {
        if s3_wake {
            return Self::S3;
        }
        let pm = LocalRegisterCopy::<u32, GenPmconA::Register>::new(saved_gen_pmcon_a);
        if pm.is_set(GenPmconA::MEM_SR) && pm.is_set(GenPmconA::DISB) {
            Self::Warm
        } else {
            Self::Cold
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_boot_mode_requires_both_mem_sr_and_disb() {
        for status in [0, 1 << 21, 1 << 23] {
            assert_eq!(
                MemoryBootMode::from_power_state(status, false),
                MemoryBootMode::Cold
            );
        }
        assert_eq!(
            MemoryBootMode::from_power_state((1 << 21) | (1 << 23), false),
            MemoryBootMode::Warm
        );
        assert_eq!(
            MemoryBootMode::from_power_state(0, true),
            MemoryBootMode::S3
        );
    }
}
