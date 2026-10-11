//! Intel model 1067x (Penryn), following coreboot's model_1067x driver.
//! Board-requested C5/C6 and Super-LFM remain gated by CPU capabilities.

use super::smm::{SmmCpu, SmrrPair, X86SaveStateFormat};
use super::{feature_control, msr_register::Msr, speedstep};
use crate::x86::mp::{CpuDriver, CpuIdMatch, CpuVendor};
use tock_registers::{LocalRegisterCopy, register_bitfields};

register_bitfields![u64,
    Cst [
        LEVEL OFFSET(0) NUMBITS(3) [], DYNAMIC_L2 OFFSET(3) NUMBITS(1) [],
        CSM_TRIGGER OFFSET(4) NUMBITS(3) [], CSTATE_ENABLE OFFSET(8) NUMBITS(1) [],
        SINGLE_STOP_GRANT OFFSET(9) NUMBITS(1) [], IO_MWAIT OFFSET(10) NUMBITS(1) [],
        COORD_DISABLE OFFSET(11) NUMBITS(1) [], DEEPER_DISABLE OFFSET(13) NUMBITS(1) [],
        DEEPER_SLEEP OFFSET(14) NUMBITS(1) [], LOCK OFFSET(15) NUMBITS(1) [],
        C6 OFFSET(25) NUMBITS(1) []
    ],
    Bbl [ L2_POLICY OFFSET(25) NUMBITS(3) [], C5 OFFSET(30) NUMBITS(2) [] ],
    Fsb [ TURBO_HYSTERESIS OFFSET(3) NUMBITS(1) [], C5_DISABLE OFFSET(31) NUMBITS(1) [] ],
    Extended [ EMTTM_DISABLE OFFSET(2) NUMBITS(1) [], SLFM_SUPPORTED OFFSET(27) NUMBITS(1) [], SLFM_ENABLE OFFSET(28) NUMBITS(1) [] ],
    Vcc [ TURBO_SUPPORTED OFFSET(63) NUMBITS(1) [] ],
    Perf [ TURBO_DISABLE OFFSET(32) NUMBITS(1) [] ],
    Platform [ EIST_DISABLE OFFSET(17) NUMBITS(1) [] ],
    Misc [
        TM1 OFFSET(3) NUMBITS(1) [], FERR OFFSET(10) NUMBITS(1) [], TM2 OFFSET(13) NUMBITS(1) [],
        EIST OFFSET(16) NUMBITS(1) [], PROCHOT OFFSET(17) NUMBITS(1) [], MWAIT OFFSET(18) NUMBITS(1) [],
        EIST_LOCK OFFSET(20) NUMBITS(1) [], C2E OFFSET(26) NUMBITS(1) [], C4E OFFSET(32) NUMBITS(1) [],
        HARD_C4E OFFSET(33) NUMBITS(1) [], EMTTM OFFSET(36) NUMBITS(1) [], TURBO_DISABLE OFFSET(38) NUMBITS(1) []
    ],
    Pic [ BYPASS OFFSET(4) NUMBITS(1) [], TM2_FALLBACK OFFSET(20) NUMBITS(1) [], TM1_LOCK OFFSET(21) NUMBITS(1) [], QUAD OFFSET(31) NUMBITS(1) [] ],
    Peci [ ENABLE OFFSET(0) NUMBITS(1) [] ],
    IoBase [ BASE OFFSET(0) NUMBITS(16) [], COORD_PORT OFFSET(16) NUMBITS(16) [] ],
    IoCapture [ BASE OFFSET(0) NUMBITS(16) [], RANGE OFFSET(16) NUMBITS(16) [] ],
    Emttm [ CONTROL OFFSET(0) NUMBITS(16) [], LOCK OFFSET(31) NUMBITS(1) [] ]
];

#[derive(Clone, Copy, Debug, Default)]
pub struct PowerPolicy {
    pub c5: bool,
    pub c6: bool,
    pub slfm: bool,
}

pub struct PenrynCpuDriver {
    pmbase: u32,
    microcode: Option<&'static [u8]>,
    power: PowerPolicy,
}

impl PenrynCpuDriver {
    pub fn new(pmbase: u32, microcode: Option<&'static [u8]>, power: PowerPolicy) -> Self {
        Self {
            pmbase,
            microcode,
            power,
        }
    }
}

static IDS: &[CpuIdMatch] = &[
    CpuIdMatch {
        vendor: CpuVendor::Intel,
        signature: 0x10676,
        mask: CpuIdMatch::EXACT_MASK,
    },
    CpuIdMatch {
        vendor: CpuVendor::Intel,
        signature: 0x10677,
        mask: CpuIdMatch::EXACT_MASK,
    },
    CpuIdMatch {
        vendor: CpuVendor::Intel,
        signature: 0x1067a,
        mask: CpuIdMatch::EXACT_MASK,
    },
];

fn cstate_support(power: PowerPolicy, bbl: u64, fsb: u64, sub: u32) -> (bool, bool) {
    let c5 = power.c5
        && LocalRegisterCopy::<u64, Bbl::Register>::new(bbl).read(Bbl::C5) != 0
        && !LocalRegisterCopy::<u64, Fsb::Register>::new(fsb).is_set(Fsb::C5_DISABLE);
    (c5, power.c6 && (sub >> 24) & 0xf != 0 && c5)
}

fn cstate_bits(current: u64, quad: bool, c5: bool, c6: bool) -> u64 {
    let mut value = LocalRegisterCopy::<u64, Cst::Register>::new(current);
    value.modify(Cst::SINGLE_STOP_GRANT::CLEAR + Cst::CSTATE_ENABLE::SET);
    if quad {
        value.modify(Cst::LEVEL.val(4));
    }
    if c5 {
        value.modify(
            Cst::DEEPER_DISABLE::CLEAR
                + Cst::LEVEL.val(0)
                + Cst::DYNAMIC_L2::SET
                + Cst::DEEPER_SLEEP::SET,
        );
    }
    value.modify(Cst::CSM_TRIGGER.val(0) + Cst::IO_MWAIT::SET);
    if c6 {
        value.modify(Cst::C6::SET);
    }
    value.get()
}

fn misc_bits(current: u64, sub: u32, eist: bool, tm2: bool, emttm: bool, turbo: bool) -> u64 {
    let mut value = LocalRegisterCopy::<u64, Misc::Register>::new(current);
    value.modify(Misc::TM1::SET + Misc::PROCHOT::SET + Misc::MWAIT::SET + Misc::FERR::SET);
    if tm2 {
        value.modify(Misc::TM2::SET);
    }
    if eist {
        value.modify(Misc::EIST::SET);
    }
    if (sub >> 8) & 0xf >= 2 {
        value.modify(Misc::C2E::SET);
    }
    if (sub >> 16) & 0xf >= 2 {
        value.modify(Misc::C4E::SET + Misc::HARD_C4E::SET);
    }
    if emttm {
        value.modify(Misc::EMTTM::SET);
    }
    if turbo {
        value.modify(Misc::TURBO_DISABLE::CLEAR);
    }
    value.get()
}

impl SmmCpu for PenrynCpuDriver {
    fn smm_save_state_format(&self) -> X86SaveStateFormat {
        X86SaveStateFormat::IntelEm64t
    }
    fn smrr_pair(&self) -> Option<SmrrPair> {
        Some(SmrrPair::Core2Alternative)
    }
}

impl CpuDriver for PenrynCpuDriver {
    fn name(&self) -> &'static str {
        "Intel Penryn"
    }
    fn id_table(&self) -> &'static [CpuIdMatch] {
        IDS
    }
    fn update_microcode(&self) {
        if let Some(blob) = self.microcode {
            // SAFETY: the board supplies a firmware-backed microcode blob.
            unsafe { super::microcode::update_current_cpu_logged(blob) };
        }
    }
    fn init_cpu(&self) {
        let (signature, count, features, _) = crate::x86::cpuid(1);
        let cores = (count >> 16) & 0xff;
        let quad = cores > 2;
        let sub = crate::x86::cpuid(5).3;
        let cst = Msr::<Cst::Register>::new(0xe2);
        let bbl = Msr::<Bbl::Register>::new(0x11e);
        let fsb = Msr::<Fsb::Register>::new(0xcd);
        let extended = Msr::<Extended::Register>::new(0xee);
        let vcc = Msr::<Vcc::Register>::new(0xce);
        let perf = Msr::<Perf::Register>::new(0x199);
        let misc = Msr::<Misc::Register>::new(0x1a0);
        // SAFETY: MP matching admits only model 1067x, whose documented MSRs
        // are used here. Microcode is updated before the erratum-sensitive PECI write.
        unsafe {
            let (c5, c6) = cstate_support(self.power, bbl.read().get(), fsb.read().get(), sub);
            let emttm = cores <= 1 || !extended.read().is_set(Extended::EMTTM_DISABLE);
            let eist = features & (1 << 7) != 0
                && !Msr::<Platform::Register>::new(0x17)
                    .read()
                    .is_set(Platform::EIST_DISABLE);
            let tm2 = eist && features & (1 << 8) != 0;
            let turbo = vcc.read().is_set(Vcc::TURBO_SUPPORTED);
            cst.write(cstate_bits(cst.read().get(), quad, c5, c6));
            // PMB0 is PMBASE+0x10. Model 1067x uses MSRs e3/e4 (not e4/e7).
            Msr::<IoBase::Register>::new(0xe3).write(
                (IoBase::BASE.val(u64::from(self.pmbase + 0x14)) + IoBase::COORD_PORT.val(0x809))
                    .value,
            );
            Msr::<IoCapture::Register>::new(0xe4).write(
                (IoCapture::BASE.val(u64::from(self.pmbase + 0x14))
                    + IoCapture::RANGE.val(if c6 {
                        4
                    } else if c5 {
                        3
                    } else {
                        2
                    }))
                .value,
            );
            if c5 {
                let mut value = bbl.read();
                value.modify(Bbl::L2_POLICY.val(2) + Bbl::C5.val(1));
                bbl.write(value.get());
            }
            let mut value = extended.read();
            if self.power.slfm && value.is_set(Extended::SLFM_SUPPORTED) {
                value.modify(Extended::SLFM_ENABLE::SET);
            }
            extended.write(value.get());
            if turbo {
                if signature & 0xf == 0xa && cores < 4 {
                    let mut value = fsb.read();
                    value.modify(Fsb::TURBO_HYSTERESIS::SET);
                    fsb.write(value.get());
                }
                let mut value = perf.read();
                value.modify(Perf::TURBO_DISABLE::CLEAR);
                perf.write(value.get());
            }
            let mut value = cst.read();
            value.modify(Cst::COORD_DISABLE::CLEAR + Cst::LOCK::SET);
            cst.write(value.get());
            if emttm {
                let states = speedstep::current_pstates();
                for (index, control) in speedstep::emttm_entries(&states).into_iter().enumerate() {
                    Msr::<Emttm::Register>::new(0xa8 + index as u32).write(u64::from(control));
                }
                let table = Msr::<Emttm::Register>::new(0xad);
                let mut value = table.read();
                value.modify(Emttm::LOCK::SET);
                table.write(value.get());
            }
            let enabled = misc_bits(misc.read().get(), sub, eist, tm2, emttm, turbo);
            misc.write(enabled);
            if eist {
                misc.write(enabled | Misc::EIST_LOCK::SET.value);
            }
            let peci = Msr::<Peci::Register>::new(0x5a0);
            let mut value = peci.read();
            value.modify(Peci::ENABLE::SET);
            peci.write(value.get());
            let pic = Msr::<Pic::Register>::new(0x1aa);
            let mut value = pic.read();
            value.modify(Pic::QUAD.val(u64::from(quad)) + Pic::TM1_LOCK::SET + Pic::BYPASS::SET);
            if tm2 {
                value.modify(Pic::TM2_FALLBACK::SET);
            }
            pic.write(value.get());
            feature_control::enable_and_lock(SmrrPair::Core2Alternative.feature_control_bits());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::x86::mp::CpuIdentity;
    #[test]
    fn penryn_matches_and_uses_alternative_smrr() {
        let driver = PenrynCpuDriver::new(0x500, None, PowerPolicy::default());
        for signature in [0x10676, 0x10677, 0x1067a] {
            assert!(driver.matches(CpuIdentity {
                vendor: CpuVendor::Intel,
                signature
            }));
        }
        assert!(!driver.matches(CpuIdentity {
            vendor: CpuVendor::Intel,
            signature: 0x6fb
        }));
        assert_eq!(driver.smrr_pair(), Some(SmrrPair::Core2Alternative));
    }
    #[test]
    fn capability_gates_and_register_values_match_coreboot() {
        let power = PowerPolicy {
            c5: true,
            c6: true,
            slfm: true,
        };
        assert_eq!(cstate_support(power, 1 << 30, 0, 1 << 24), (true, true));
        assert_eq!(
            cstate_support(power, 1 << 30, 1 << 31, 1 << 24),
            (false, false)
        );
        assert_eq!(cstate_support(power, 0, 0, 1 << 24), (false, false));
        assert_eq!(cstate_bits(0, false, true, true), 0x2004508);
        assert_eq!(cstate_bits(0, true, false, false), 0x504);
        let bits = misc_bits(0, 0x20200, false, false, false, false);
        assert_eq!(bits & ((1 << 13) | (1 << 16) | (1 << 20)), 0);
        assert_ne!(bits & (1 << 26), 0);
        assert_ne!(bits & (1 << 32), 0);
    }
}
