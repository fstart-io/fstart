// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native Skylake pmc.c power/wake policy. PCH-H only, not a thermal policy.
use super::pmio::AcpiIo;
use super::{
    gpio::{self, Group},
    hsio,
    pcr::{self, Pcr, Port},
    pmc::*,
};
use crate::southbridge::pmio_ich::PM_CONTROL;
use fstart_pci::EcamDevice;
use tock_registers::{
    LocalRegisterCopy,
    interfaces::{ReadWriteable, Readable, Writeable},
};

#[repr(u8)]
#[derive(Debug, Clone, Copy)]
pub enum S3Assert {
    Us60 = 0,
    Ms1 = 1,
    Ms50 = 2,
    Seconds2 = 3,
}
#[derive(Debug, Clone, Copy)]
pub enum S4Assert {
    Pch,
    Seconds1,
    Seconds2,
    Seconds3,
    Seconds4,
}
#[repr(u8)]
#[derive(Debug, Clone, Copy)]
pub enum SusAssert {
    Off = 0,
    Ms500 = 1,
    Seconds1 = 2,
    Seconds4 = 3,
}
#[repr(u8)]
#[derive(Debug, Clone, Copy)]
pub enum AAssert {
    Off = 0,
    Seconds4 = 1,
    Ms98 = 2,
    Seconds2 = 3,
}
#[repr(u8)]
#[derive(Debug, Clone, Copy)]
pub enum PowerCycle {
    Seconds4 = 0,
    Seconds3 = 1,
    Seconds2 = 2,
    Seconds1 = 3,
}
#[derive(Debug, Clone, Copy)]
pub enum DeepSleep {
    Legacy {
        s3_ac: bool,
        s3_dc: bool,
        s5_ac: bool,
        s5_dc: bool,
    },
    S5Dc,
    S5AcDc,
    S4S5Dc,
    S4S5AcDc,
    AllDc,
    AllAcDc,
}
#[derive(Debug, Clone, Copy)]
pub struct Policy {
    pub after_power_loss: AfterPowerLoss,
    pub gpe: [Group; 3],
    pub s3_assert: S3Assert,
    pub s4_assert: S4Assert,
    pub sus_assert: SusAssert,
    pub a_assert: AAssert,
    pub power_cycle: PowerCycle,
    pub stretch_after_suspend_loss: bool,
    pub button_override: u8,
    pub s0ix: bool,
    pub wol_override: bool,
    pub wowlan: bool,
    pub wowlan_deep: bool,
    pub deep_sleep: DeepSleep,
    pub lan_wake: bool,
    pub disable_ac_present_pulldown: bool,
    /// Includes native WakeConfigPcieWakeFromDeepSx.
    pub wake_pin: bool,
    pub disable_pmc_data_bar: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Policy,
    Device,
    Decode,
    Locked,
    Readback,
    Pcr(pcr::Error),
    Gpio(gpio::Error),
    Hsio(hsio::Error),
}
impl From<pcr::Error> for Error {
    fn from(e: pcr::Error) -> Self {
        Self::Pcr(e)
    }
}
impl From<gpio::Error> for Error {
    fn from(e: gpio::Error) -> Self {
        Self::Gpio(e)
    }
}
impl From<hsio::Error> for Error {
    fn from(e: hsio::Error) -> Self {
        Self::Hsio(e)
    }
}

impl Policy {
    pub fn validate(self) -> Result<(), Error> {
        if self.button_override > 7
            || self.wowlan_deep && !self.wowlan
            || self.gpe[0] == self.gpe[1]
            || self.gpe[0] == self.gpe[2]
            || self.gpe[1] == self.gpe[2]
        {
            return Err(Error::Policy);
        }
        Ok(())
    }
    fn deep_masks(self) -> (u32, u32, u32) {
        match self.deep_sleep {
            DeepSleep::Legacy {
                s3_ac,
                s3_dc,
                s5_ac,
                s5_dc,
            } => (
                u32::from(s3_ac) | (u32::from(s3_dc) << 1),
                u32::from(s5_ac) | (u32::from(s5_dc) << 1),
                (u32::from(s5_ac) << 14) | (u32::from(s5_dc) << 15),
            ),
            DeepSleep::S5Dc => (0, 0, 1 << 15),
            DeepSleep::S5AcDc => (0, 0, 3 << 14),
            DeepSleep::S4S5Dc => (0, 2, 1 << 15),
            DeepSleep::S4S5AcDc => (0, 3, 3 << 14),
            DeepSleep::AllDc => (2, 2, 1 << 15),
            DeepSleep::AllAcDc => (3, 3, 3 << 14),
        }
    }
}

/// # Safety
/// Actual exclusively owned PCH-H 64-KiB PWRM, PCI and PCR mappings; this is
/// the authenticated cold successor's one-shot device-init phase. Saved wake
/// and reset observations have already been consumed before status clear.
/// Call after RTC setup, before SMM installation. The real installer later
/// disables SCI only after installing its actual APM handler; do NOT issue an
/// early APM command to a not-yet-installed handler. ACPI timer remains live.
/// Any error after writes is fatal partial setup, not a rollback/retry token.
pub unsafe fn configure_native(
    pmc: &PmcRegisters,
    device: &EcamDevice,
    lpc: &EcamDevice,
    pcr: &Pcr,
    policy: Policy,
) -> Result<(), Error> {
    policy.validate()?;
    let silicon = hsio::Silicon::read(lpc)?;
    if silicon.kind() != super::dmi::Kind::H {
        return Err(Error::Device);
    }
    require_decodes(device).map_err(|_| Error::Decode)?;
    let a = unsafe { device.register::<u32, GenPmconA::Register>(0xa0) };
    let b = unsafe { device.register::<u32, GenPmconB::Register>(0xa4) };
    let reset = unsafe { device.register::<u32, ExtendedReset::Register>(0xac) };
    // After a reboot without power loss the locks set below by the previous
    // boot are still in place. Like native pmc.c, write anyway: the hardware
    // ignores writes to locked fields, and the stretch-locked ones are
    // checked against this policy once written.
    let stretch_locked = b.is_set(GenPmconB::STRETCH_LOCK);
    if reset.is_set(ExtendedReset::GLOBAL_RESET) {
        return Err(Error::Policy);
    }
    let espi = pcr.register::<()>(Port::EspiSpi, 0xc00c)?.get() & 1 != 0;
    let mut usb3 = 0u32;
    for lane in 0..=9 {
        if silicon.lane_owner(pcr, lane)? == 1 {
            usb3 |= 1 << lane;
        }
    }
    // Native after-G3 byte access preserves the RTC W0C bit and never touches
    // upper reset-status fields. Last follows the current active state here.
    let low = unsafe { device.register::<u8, ()>(0xa4) };
    let mut after = LocalRegisterCopy::<u32, GenPmconB::Register>::new(u32::from(low.get()));
    after.modify(
        GenPmconB::SLEEP_AFTER_POWER_FAIL
            .val(u32::from(policy.after_power_loss == AfterPowerLoss::Off)),
    );
    low.set(after.get() as u8);
    pmc.configure_gpe_routes(pcr, policy.gpe)?;
    let (s4, s4_enable) = match policy.s4_assert {
        S4Assert::Pch => (0, 0),
        S4Assert::Seconds1 => (3, 1),
        S4Assert::Seconds2 => (2, 1),
        S4Assert::Seconds3 => (1, 1),
        S4Assert::Seconds4 => (0, 1),
    };
    b.modify(
        GenPmconB::S4_ASSERT_WIDTH.val(s4)
            + GenPmconB::S4_ASSERT_ENABLE.val(s4_enable)
            + GenPmconB::S3_ASSERT_WIDTH.val(policy.s3_assert as u32)
            + GenPmconB::DISABLE_SUS_STRETCH.val(u32::from(!policy.stretch_after_suspend_loss))
            + GenPmconB::POWER_FAIL::CLEAR
            + GenPmconB::HOST_RESET::CLEAR
            + GenPmconB::SUSPEND_POWER_FAIL::CLEAR,
    );
    pmc.power_config.modify(
        PowerConfig::SLP_SUS_ASSERT.val(policy.sus_assert as u32)
            + PowerConfig::SLP_A_ASSERT.val(policy.a_assert as u32)
            + PowerConfig::POWER_CYCLE.val(policy.power_cycle as u32),
    );
    if stretch_locked
        && (b.read(GenPmconB::S4_ASSERT_WIDTH) != s4
            || b.read(GenPmconB::S4_ASSERT_ENABLE) != s4_enable
            || b.read(GenPmconB::S3_ASSERT_WIDTH) != policy.s3_assert as u32
            || pmc.power_config.read(PowerConfig::SLP_SUS_ASSERT) != policy.sus_assert as u32
            || pmc.power_config.read(PowerConfig::SLP_A_ASSERT) != policy.a_assert as u32
            || pmc.power_config.read(PowerConfig::POWER_CYCLE) != policy.power_cycle as u32)
    {
        // Locked by an earlier boot with a different policy.
        return Err(Error::Locked);
    }
    a.modify(
        GenPmconA::GLOBAL_RESET_STATUS::CLEAR
            + GenPmconA::MS4V::CLEAR
            + GenPmconA::OPI_PLL_SHUTDOWN::SET
            + GenPmconA::SPXB_CLOCK_GATE::SET
            + GenPmconA::L1_C0::SET
            + GenPmconA::L1_OPI::SET
            + GenPmconA::L1_BCLKREQ::SET,
    );
    unsafe { device.register::<u32, BmControl::Register>(0xa8) }.modify(
        BmControl::HOLD_BLOCK::SET + BmControl::ZERO_ENABLE::SET + BmControl::SYNC_MSG::SET,
    );
    // The following opaque clock/event recipe words are literal native PMC
    // values, not measured energy/thermal results or a board fan policy.
    pmc.power_management
        .set(pmc.power_management.get() | 0x0105_2fff);
    pmc.config2.modify(
        Config2::BUTTON_BEHAVIOR.val(3)
            + Config2::BUTTON_DISABLE::CLEAR
            + Config2::BUTTON_OVERRIDE.val(u32::from(policy.button_override)),
    );
    for (r, v) in pmc
        .non_snoop
        .iter()
        .zip([0x1e2dd701, 1, 0x1e2dd701, 1, 0x1c000])
    {
        r.set(v);
    }
    pmc.non_snoop_sa.set(0x04043400);
    for (r, v) in pmc
        .clock_wake
        .iter()
        .zip([0xbf8ff701, 0x0201c7e1, 0x3f8ff701, 0x0201c7e1])
    {
        r.set(v);
    }
    pmc.clock_wake_cf.set(0x1c000);
    for (r, v) in pmc
        .power_ack
        .iter()
        .zip([0x00181300, 0x02006001, 0x00181300, 0x02006001])
    {
        r.set(v);
    }
    pmc.misc_event.set(0x00060a00);
    pmc.sync_state_hysteresis.set(0x00200840);
    pmc.sync_mode.set(0x7000f4e6);
    pmc.power_config3
        .set(if espi { 0x01040009 } else { 0x01040001 });
    pmc.down_ppb.set(0x16bf0003);
    pmc.power_config4
        .modify(PowerConfig4::RAMP_DURATION.val(0x18) + PowerConfig4::USB2_PHY_GATE::SET);
    pmc.sync_c0.set(0x7000f4c0);
    pmc.power_config.modify(PowerConfig::ENERGY_REPORT::SET);
    pmc.power_config.modify(PowerConfig::ENERGY_REPORT2::SET); // two native publications
    pmc.modphy_config[0].set(pmc.modphy_config[0].get() & !usb3);
    pmc.modphy_config[1].set(pmc.modphy_config[1].get() & !0x3f00_0000);
    for (r, h) in [
        (&pmc.clock_gate1, 6),
        (&pmc.clock_gate2, 8),
        (&pmc.clock_gate3, 0x29),
    ] {
        r.modify(GatingPolicy::HYSTERESIS.val(h) + GatingPolicy::ENABLE::SET);
    }
    pmc.obff.set(0x1e0a4616);
    let cppm = (1 << 29)
        | (1 << 27)
        | (1 << 25)
        | (1 << 24)
        | (1 << 23)
        | (1 << 22)
        | (1 << 14)
        | 3
        | if policy.s0ix { 1 << 15 } else { 0 };
    pmc.cppm_control
        .set((pmc.cppm_control.get() & !((1 << 22) | (1 << 15) | (1 << 13) | 0x1ff)) | cppm);
    pmc.cppm_misc.set(0x0005db01);
    pmc.gate_misc
        .set(pmc.gate_misc.get() | (1 << 31) | (1 << 30) | (1 << 21));
    pmc.vr_misc.set(pmc.vr_misc.get() | (1 << 3));
    pmc.clock_shutdown[0].set((pmc.clock_shutdown[0].get() & !0x0070_0007) | 0x0020_0001);
    if !silicon.is_kbl_h() {
        pmc.clock_shutdown[1].set(pmc.clock_shutdown[1].get() | (1 << 25) | (1 << 17));
    }
    pmc.serial_clock[0].set((pmc.serial_clock[0].get() & !(0x1f << 6)) | 0xe000_0080);
    pmc.serial_clock[1].set((pmc.serial_clock[1].get() & !(0x1f << 6)) | 0x80);
    pmc.mphy_policy
        .set((pmc.mphy_policy.get() & !0x003f_0c00) | 0x800a_0000);
    pmc.multipower_gate.modify(
        GatingPolicy::HYSTERESIS.val(0x69) + GatingPolicy::MPG::SET + GatingPolicy::ENABLE::SET,
    );
    a.modify(
        GenPmconA::GLOBAL_RESET_STATUS::CLEAR
            + GenPmconA::MS4V::CLEAR
            + GenPmconA::LAN_SX.val(u32::from(policy.wol_override))
            + GenPmconA::LAN_AG3.val(u32::from(policy.wol_override))
            + GenPmconA::LAN_DEEP_DISABLE.val(u32::from(!policy.wol_override))
            + GenPmconA::LAN_DC_DISABLE::SET,
    );
    b.modify(
        GenPmconB::POWER_FAIL::CLEAR
            + GenPmconB::HOST_RESET::CLEAR
            + GenPmconB::SUSPEND_POWER_FAIL::CLEAR
            + GenPmconB::PME_S5_DISABLE::CLEAR
            + GenPmconB::WOL_OVERRIDE.val(u32::from(policy.wol_override)),
    );
    pmc.power_config.modify(PowerConfig::OSC_SHUTDOWN::SET);
    // Own the native cio2.c H-only branch here, exactly once. No LP CAM
    // FLS/PSF recipe or synthetic camera readiness on a PCH-H platform.
    pmc.cppm_control.modify(CppmControl::CIO2_DISABLE::SET);
    if pmc.cppm_control.get() == u32::MAX || !pmc.cppm_control.is_set(CppmControl::CIO2_DISABLE) {
        return Err(Error::Readback);
    }
    if policy.wowlan {
        pmc.power_config3.modify(
            PowerConfig3::WLAN_HOST::SET
                + PowerConfig3::WLAN_DEEP.val(u32::from(policy.wowlan_deep)),
        );
    }
    AcpiIo::new(ACPI_BASE)
        .control()
        .modify(PM_CONTROL::SLEEP_TYPE.val(0) + PM_CONTROL::SCI_EN::SET);
    let (s3, s4, s5) = policy.deep_masks();
    if matches!(policy.deep_sleep, DeepSleep::Legacy { .. }) {
        pmc.deep_s3
            .modify(DeepS34::AC.val(s3 & 1) + DeepS34::DC.val((s3 >> 1) & 1));
        pmc.deep_s4
            .modify(DeepS34::AC.val(s4 & 1) + DeepS34::DC.val((s4 >> 1) & 1));
        pmc.deep_s5
            .modify(DeepS5::AC.val((s5 >> 14) & 1) + DeepS5::DC.val((s5 >> 15) & 1));
    } else {
        pmc.deep_s3.set(s3);
        pmc.deep_s4.set(s4);
        pmc.deep_s5.set(s5);
    }
    pmc.deep_config.modify(
        DeepConfig::LAN_WAKE.val(u32::from(policy.lan_wake))
            + DeepConfig::DISABLE_AC_PULLDOWN.val(u32::from(policy.disable_ac_present_pulldown))
            + DeepConfig::WAKE_PIN.val(u32::from(policy.wake_pin)),
    );
    pmc.power_reset_status.set(0x31);
    b.modify(
        GenPmconB::POWER_FAIL::SET
            + GenPmconB::HOST_RESET::SET
            + GenPmconB::SUSPEND_POWER_FAIL::SET,
    );
    // Preserve native two-step status handling: these low-byte causes clear 0.
    low.set(
        low.get()
            & !((GenPmconB::RTC_BATTERY_DEAD::SET.value | GenPmconB::POWER_FAIL::SET.value) as u8),
    );
    for cause in &pmc.global_reset_cause {
        cause.set(cause.get());
    }
    b.modify(
        GenPmconB::POWER_FAIL::CLEAR
            + GenPmconB::HOST_RESET::CLEAR
            + GenPmconB::SUSPEND_POWER_FAIL::CLEAR
            + GenPmconB::ACPI_BASE_LOCK::SET
            + GenPmconB::STRETCH_LOCK::SET,
    );
    if policy.disable_pmc_data_bar {
        b.modify(
            GenPmconB::DATA_BAR_DISABLE::SET
                + GenPmconB::POWER_FAIL::CLEAR
                + GenPmconB::HOST_RESET::CLEAR
                + GenPmconB::SUSPEND_POWER_FAIL::CLEAR,
        );
    }
    pmc.power_config3.modify(PowerConfig3::LOCK::SET);
    reset.modify(ExtendedReset::GLOBAL_RESET_LOCK::SET);
    if !b.matches_all(GenPmconB::ACPI_BASE_LOCK::SET + GenPmconB::STRETCH_LOCK::SET)
        || !pmc.power_config3.is_set(PowerConfig3::LOCK)
        || !reset.is_set(ExtendedReset::GLOBAL_RESET_LOCK)
    {
        return Err(Error::Readback);
    }
    Ok(())
}
