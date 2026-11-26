// SPDX-License-Identifier: MIT OR Apache-2.0
//! Actual producer/failure-boundary tests. NOT joint-feedback full-flow proof.
extern crate std;
use super::*;
use crate::skylake::{
    additional_steps::OltmOverride,
    jedec::BoardType,
    spd::testing::{seal, sodimm},
};
use core::cell::UnsafeCell;

pub(crate) struct GdxcImage([UnsafeCell<u32>; 12]);
impl GdxcImage {
    pub(crate) fn new() -> Self {
        Self([const { UnsafeCell::new(0) }; 12])
    }
    pub(crate) fn registers(&self) -> &GdxcRegisters {
        unsafe { GdxcRegisters::at(self.0.as_ptr() as usize) }
    }
}
pub(crate) fn input(signature: u32) -> ColdPolicyInput {
    ColdPolicyInput {
        cpu: Cpu::from_signature(signature).unwrap(),
        capabilities: Capabilities {
            capid_a: 0,
            capid_b: 8,
            capid_c: 0,
        },
        overrides: PostSpdOverrides {
            ecc_support: true,
            rank_interleave: false,
            enhanced_interleave: false,
            vdd_voltage_done: false,
            gdxc_enable: false,
            gdxc_mot_size: 0,
            gdxc_iot_size: 0,
            lpddr_dram_odt: false,
        },
        platform_tck_min_fs: 938000,
        phy: PhyPolicy {
            board: BoardType::Desktop,
            dq_pins_interleaved: true,
            ddp_shared_clock: false,
            vdd_mv: 1200,
            vccio_mv: 950,
            rcomp_resistor: [200, 81, 162],
            rcomp_target: [100, 40, 40, 23, 40],
            vtt_odt: false,
        },
        dram_odt: true,
        ca_vref_config: 0,
        dummy_reads: false,
        thermal: OltmPolicy {
            override_mode: OltmOverride::OpenLoop,
            enable_oltm: false,
            enable_cltm: true,
            refresh_2x: 0,
            user_power_weights: false,
            ddr_thermal_sensor: true,
        },
    }
}

/// Register device model: PCODE/RCOMP/MRS/reset acknowledgements and actual
/// multicast alias effects derived ONLY from real producer writes. No preset
/// calibration/latency/MR seed and no successful phase callback is used.

#[test]
fn cold_training_package_selects_training_mode_and_write_deskew_gate() {
    for (signature, mode) in [
        (0x906e9, TrainingMode::Margin),
        (0x806e9, TrainingMode::Power),
    ] {
        let raw = sodimm();
        let inventory = [[Some(Dimm::decode(&raw).unwrap()), None], [None; 2]];
        let gdxc = GdxcImage::new();
        let policy = ColdPolicy::resolve(&inventory, gdxc.registers(), input(signature)).unwrap();
        assert_eq!(policy.training_mode(), mode);
        let gates = policy.gates();
        assert_eq!(
            gates.training_enables & (1 << 13) != 0,
            mode == TrainingMode::Margin
        );
    }
}

#[test]
fn cold_training_only_required_retries_and_last_required_is_fatal() {
    assert_eq!(retry_required(RetrainOutcome::Passed, 0), Ok(false));
    assert_eq!(retry_required(RetrainOutcome::Passed, 1), Ok(false));
    for (read, write) in [(true, false), (false, true), (true, true)] {
        assert_eq!(
            retry_required(RetrainOutcome::Required { read, write }, 0),
            Ok(true)
        );
        assert_eq!(
            retry_required(RetrainOutcome::Required { read, write }, 1),
            Err(ColdError::RetrainUnresolved { read, write })
        );
    }
}

#[test]
fn cold_training_resolve_rejects_nonstandard_ecc_and_unavailable_fuses() {
    let raw = sodimm();
    let inventory = [[Some(Dimm::decode(&raw).unwrap()), None], [None; 2]];
    let gdxc = GdxcImage::new();
    let mut p = input(0x906e9);
    p.phy.vdd_mv = 1350;
    assert!(matches!(
        ColdPolicy::resolve(&inventory, gdxc.registers(), p),
        Err(Error::InvalidPolicy)
    ));
    p = input(0x906e9);
    p.capabilities.capid_b = 0;
    assert!(matches!(
        ColdPolicy::resolve(&inventory, gdxc.registers(), p),
        Err(Error::Ddr4FusedOff)
    ));
    p.capabilities.capid_a = u32::MAX;
    assert!(matches!(
        ColdPolicy::resolve(&inventory, gdxc.registers(), p),
        Err(Error::HostBridgeUnavailable)
    ));
    let mut ecc_raw = raw;
    ecc_raw[13] |= 8;
    seal(&mut ecc_raw);
    let ecc = [[Some(Dimm::decode(&ecc_raw).unwrap()), None], [None; 2]];
    assert!(matches!(
        ColdPolicy::resolve(&ecc, gdxc.registers(), input(0x906e9)),
        Err(Error::EccRequired)
    ));
}
