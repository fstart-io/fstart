// SPDX-License-Identifier: MIT OR Apache-2.0
//! The SMU v14 feature controls: what the SMU firmware reports as its
//! defaults ([`GET_SMU_FEATURE_DEFAULTS`], `11h`), the platform's overrides
//! of single bits, and what is enabled with them ([`ENABLE_SMU_FEATURES`],
//! `03h`).
//!
//! The vendor (AmdNbioDxe `SmuFixupPlatformConfigSP7`, Dell `0xfc50`,
//! GIGABYTE `0x9ffc`; instruction-read) asks socket 0's SMU for its
//! defaults, overrides bits 19, 21, 22, 28 and 30 of the standard word and
//! bit 0 of the extended word from PCDs (`0xFD` = keep), and stores the
//! three words in `PcdSmuFeatureControlDefines{,Ext,64}`. Every later
//! user reads them from there: `SmuInitialization` sends them
//! ([`enable`]), `SmuMpioClockGatingRTBCallBack` takes bits 20 and 14 for
//! `MPIO_ENABLE_CG` ([`Controls::mpio_clock_gating`]), CPPC bit 22.

use fstart_arch::x86::cpu::msr::{Msr, MsrAccess};
use tock_registers::interfaces::Readable;
use tock_registers::register_bitfields;

use super::msg::{ENABLE_SMU_FEATURES, GET_SMU_FEATURE_DEFAULTS};
use crate::smu::{Error, SmuMessages};

register_bitfields![u64,
    /// MSR `C001_0C02` (64-bit), the one bit `SmuInitialization` reads
    /// (Dell `0x3aa0`, GIGABYTE `0x34cd`; its name taken from the PCD that
    /// stands in for it, `PcdAmdDrbStatusCleared`).
    pub Drb [
        STATUS_CLEARED OFFSET(38) NUMBITS(1) []
    ]
];

/// MSR `C001_0C02`.
pub const DRB: Msr<Drb::Register> = Msr::new(0xc001_0c02);

/// Standard-word bits the platform overrides or reads.
pub mod bit {
    /// `MPIO_ENABLE_CG` argument 1.
    pub const MPIO_CG_1: u32 = 1 << 14;
    /// Cleared before [`super::enable`] when DRB status is cleared.
    pub const DRB: u32 = 1 << 17;
    /// `PcdDfCstateEnable`.
    pub const DF_CSTATE: u32 = 1 << 19;
    /// `MPIO_ENABLE_CG` argument 0.
    pub const MPIO_CG_0: u32 = 1 << 20;
    /// `PcdVRHOTEnable`.
    pub const VR_HOT: u32 = 1 << 21;
    /// `PcdCfgCPPCMode`: CPPC (the `_CPC` queries need it).
    pub const CPPC: u32 = 1 << 22;
    /// `PcdAmdCxlQos`.
    pub const CXL_QOS: u32 = 1 << 28;
    /// `PcdCfgDiagnosticMode`.
    pub const DIAGNOSTIC: u32 = 1 << 30;
    /// Extended word: `PcdAmdVddmDiDtCtrl`.
    pub const EXT_VDDM_DIDT: u32 = 1 << 0;
    /// Standard word: `UpdatePccLimit` (`58h`) is sent when set (Dell only).
    pub const PCC_LIMIT: u32 = 1 << 5;
}

/// The three feature words (`PcdSmuFeatureControlDefines`, `...Ext`,
/// `...64`): the reply of [`GET_SMU_FEATURE_DEFAULTS`] in its argument
/// order, then [`EnableSmuFeatures`](ENABLE_SMU_FEATURES)'s.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Controls {
    pub standard: u32,
    pub extended: u32,
    pub standard64: u32,
}

/// The single-bit overrides; `None` keeps the SMU's default (the PCDs'
/// `0xFD`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Overrides {
    pub df_cstate: Option<bool>,
    pub vr_hot: Option<bool>,
    pub cppc: Option<bool>,
    pub diagnostic: Option<bool>,
    pub cxl_qos: Option<bool>,
    pub vddm_didt: Option<bool>,
}

impl Overrides {
    /// Both donors: every PCD `0xFD` (`PcdDfCstateEnable` Dell token
    /// 0x248 / GIGABYTE ex `0x51000`, `PcdVRHOTEnable` 0x2A3 / `0x4F0DA`,
    /// `PcdCfgCPPCMode` 0x218 / `0x500BA`, `PcdCfgDiagnosticMode` 0x21C /
    /// `0x4F01E`, `PcdAmdCxlQos` 0x16E / `0x500CF`, `PcdAmdVddmDiDtCtrl`
    /// 0x32 / `0xCCA11`).
    pub const NONE: Self = Self {
        df_cstate: None,
        vr_hot: None,
        cppc: None,
        diagnostic: None,
        cxl_qos: None,
        vddm_didt: None,
    };
}

const fn set(word: u32, mask: u32, value: Option<bool>) -> u32 {
    match value {
        Some(true) => word | mask,
        Some(false) => word & !mask,
        None => word,
    }
}

impl Controls {
    /// The words with `overrides` applied (`SmuFixupPlatformConfigSP7`).
    pub const fn with(self, o: &Overrides) -> Self {
        let mut standard = self.standard;
        standard = set(standard, bit::DF_CSTATE, o.df_cstate);
        standard = set(standard, bit::VR_HOT, o.vr_hot);
        standard = set(standard, bit::CPPC, o.cppc);
        standard = set(standard, bit::DIAGNOSTIC, o.diagnostic);
        standard = set(standard, bit::CXL_QOS, o.cxl_qos);
        Self {
            standard,
            extended: set(self.extended, bit::EXT_VDDM_DIDT, o.vddm_didt),
            standard64: self.standard64,
        }
    }

    /// The words [`enable`] sends: the standard word's bit 17 cleared when
    /// `drb_status_cleared` (`PcdAmdDrbStatusCleared`, else MSR
    /// [`DRB`]`[38]`; Dell `0x3b00`, GIGABYTE `0x34cd`).
    pub const fn to_enable(self, drb_status_cleared: bool) -> Self {
        let standard = if drb_status_cleared {
            self.standard & !bit::DRB
        } else {
            self.standard
        };
        Self { standard, ..self }
    }

    /// `MPIO_ENABLE_CG`'s three arguments (`SmuMpioClockGatingRTBCallBack`,
    /// Dell AmdNbioDxe `0x4ed0`): standard bits 20 and 14, and the unused
    /// tile clock gating.
    pub const fn mpio_clock_gating(&self, unused_tile: bool) -> [bool; 3] {
        [
            self.standard & bit::MPIO_CG_0 != 0,
            self.standard & bit::MPIO_CG_1 != 0,
            unused_tile,
        ]
    }

    /// CPPC on (standard bit 22).
    pub const fn cppc(&self) -> bool {
        self.standard & bit::CPPC != 0
    }
}

/// Whether this CPU reports DRB status cleared (MSR [`DRB`]`[38]`).
pub fn drb_status_cleared<M: MsrAccess + ?Sized>(cpu: &M) -> bool {
    cpu.reg(DRB).is_set(Drb::STATUS_CLEARED)
}

/// [`GET_SMU_FEATURE_DEFAULTS`]: argument 3 is 1 for the emulation
/// platforms the vendor detects (`PcdAmdPreSilCtrl0`), 0 on silicon.
pub fn defaults<M: SmuMessages + ?Sized>(smu: &M, emulation: bool) -> Result<Controls, Error> {
    let mut args = [0, 0, 0, u32::from(emulation), 0, 0];
    smu.send(GET_SMU_FEATURE_DEFAULTS, &mut args)?;
    Ok(Controls {
        standard: args[0],
        extended: args[1],
        standard64: args[2],
    })
}

/// [`ENABLE_SMU_FEATURES`] with `controls` (standard, extended,
/// standard-64), as `SmuInitialization` sends it (Dell `0x3b90`).
pub fn enable<M: SmuMessages + ?Sized>(smu: &M, controls: Controls) -> Result<(), Error> {
    let mut args = [
        controls.standard,
        controls.extended,
        controls.standard64,
        0,
        0,
        0,
    ];
    smu.send(ENABLE_SMU_FEATURES, &mut args)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::smu::ARGUMENT_COUNT;
    use core::cell::RefCell;
    use std::vec::Vec;

    /// Answers `11h` with fixed words and records every message.
    #[derive(Default)]
    pub(crate) struct Smu {
        pub reply: [u32; 3],
        pub sent: RefCell<Vec<(u32, [u32; ARGUMENT_COUNT])>>,
    }

    impl SmuMessages for Smu {
        fn send(&self, id: u32, args: &mut [u32; ARGUMENT_COUNT]) -> Result<(), Error> {
            self.sent.borrow_mut().push((id, *args));
            if id == GET_SMU_FEATURE_DEFAULTS {
                args[..3].copy_from_slice(&self.reply);
            }
            Ok(())
        }
    }

    #[test]
    fn defaults_overrides_and_the_enable_words() {
        let smu = Smu {
            reply: [bit::MPIO_CG_0 | bit::DRB | bit::CPPC | 0x7, 0x3, 0x55],
            ..Smu::default()
        };
        let c = defaults(&smu, false).unwrap();
        assert_eq!(smu.sent.borrow()[0], (0x11, [0; 6]));
        assert_eq!(c.with(&Overrides::NONE), c, "0xFD everywhere: kept");
        assert_eq!(c.mpio_clock_gating(true), [true, false, true]);
        assert!(c.cppc());
        let o = Overrides {
            cppc: Some(false),
            vr_hot: Some(true),
            vddm_didt: Some(false),
            ..Overrides::NONE
        };
        let c2 = c.with(&o);
        assert_eq!(c2.standard, c.standard & !bit::CPPC | bit::VR_HOT);
        assert_eq!(c2.extended, 0x2);
        assert_eq!(c.to_enable(false), c);
        assert_eq!(c.to_enable(true).standard, c.standard & !bit::DRB);
        enable(&smu, c.to_enable(true)).unwrap();
        assert_eq!(
            smu.sent.borrow()[1],
            (0x03, [bit::MPIO_CG_0 | bit::CPPC | 0x7, 0x3, 0x55, 0, 0, 0])
        );
    }

    #[test]
    fn drb_is_msr_bit_38() {
        use fstart_arch::x86::cpu::msr::testing::MsrSpace;
        assert!(drb_status_cleared(&MsrSpace::new([(0xc001_0c02, 1 << 38)])));
        assert!(!drb_status_cleared(&MsrSpace::new([(
            0xc001_0c02,
            1 << 37
        )])));
    }
}
