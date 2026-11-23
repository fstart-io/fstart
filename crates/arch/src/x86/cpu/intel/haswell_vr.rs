//! Haswell package voltage-regulator policy.

use crate::x86::cpu::msr::{Msr, MsrAccess, NativeMsr};
use tock_registers::{
    interfaces::{ReadWriteable, Readable, Writeable},
    register_bitfields,
};

register_bitfields![u64,
    /// MSR_VR_CURRENT_CONFIG (0x601): PSI current thresholds in amps.
    VrCurrentConfig [
        PSI1 OFFSET(32) NUMBITS(10) [],
        PSI2 OFFSET(42) NUMBITS(10) [],
        PSI3 OFFSET(52) NUMBITS(10) [],
        PSI4 OFFSET(62) NUMBITS(1) []
    ],
    /// MSR_VR_MISC_CONFIG (0x603): current slope/offset, minimum VID, and idle ramps.
    VrMiscConfig [
        MIN_VID OFFSET(24) NUMBITS(8) [],
        OFFSET_I OFFSET(32) NUMBITS(8) [],
        SLOPE OFFSET(40) NUMBITS(10) [],
        FAST_EXIT OFFSET(50) NUMBITS(1) [],
        FAST_ENTRY OFFSET(51) NUMBITS(1) [],
        DECAY OFFSET(52) NUMBITS(1) [],
        SLOW_RAMP OFFSET(53) NUMBITS(2) []
    ]
];

const MSR_VR_CURRENT_CONFIG: Msr<VrCurrentConfig::Register> = Msr::new(0x601);
const MSR_VR_MISC_CONFIG: Msr<VrMiscConfig::Register> = Msr::new(0x603);
const MSR_VR_MISC_CONFIG2: Msr = Msr::new(0x636);

/// Haswell-ULT fast-ramp voltage/time encoding (coreboot `VR_MISC_CONFIG2`).
const ULT_FAST_RAMP_DEFAULT: u64 = 0x006f;

/// Package voltage-regulator policy supplied by the platform.
#[derive(Clone, Copy, Debug, Default)]
pub struct VrConfig {
    /// Minimum VID floor written to `MSR_VR_MISC_CONFIG[31:24]`.
    pub min_vid: u8,
    /// Slow-ramp rate override for Haswell-ULT (`None` enables fast exit).
    pub slow_ramp: Option<u8>,
}

impl VrConfig {
    /// Apply package VR policy on the Haswell BSP during package power setup.
    ///
    /// # Safety
    ///
    /// Must run once on a Haswell BSP.
    pub unsafe fn apply(self, ult: bool) {
        // SAFETY: caller guarantees Haswell BSP.
        self.program(&unsafe { NativeMsr::new() }, ult);
    }

    fn program(self, cpu: &impl MsrAccess, ult: bool) {
        let mut psi = VrCurrentConfig::PSI1.val(20)
            + VrCurrentConfig::PSI2.val(5)
            + VrCurrentConfig::PSI3.val(1);
        if ult {
            psi += VrCurrentConfig::PSI4::SET;
        }
        cpu.reg(MSR_VR_CURRENT_CONFIG).modify(psi);

        let mut config = VrMiscConfig::MIN_VID.val(self.min_vid.into())
            + VrMiscConfig::OFFSET_I::CLEAR
            + VrMiscConfig::SLOPE.val(512)
            + VrMiscConfig::FAST_EXIT.val(u64::from(!ult || self.slow_ramp.is_none()))
            + VrMiscConfig::FAST_ENTRY::CLEAR
            + VrMiscConfig::DECAY::SET;
        if ult {
            config += VrMiscConfig::SLOW_RAMP.val(u64::from(self.slow_ramp.unwrap_or(1)));
        }
        cpu.reg(MSR_VR_MISC_CONFIG).modify(config);

        if ult {
            let reg = cpu.reg(MSR_VR_MISC_CONFIG2);
            reg.set((reg.get() & !0xffff) | ULT_FAST_RAMP_DEFAULT);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::x86::cpu::msr::testing::MsrSpace;

    #[test]
    fn single_composed_vr_writes() {
        for ult in [false, true] {
            let cpu = MsrSpace::new([(0x601, 1), (0x603, 2)]);
            VrConfig {
                min_vid: 4,
                slow_ramp: Some(3),
            }
            .program(&cpu, ult);
            let m = cpu.borrow();
            assert_eq!(m.writes.len(), 2 + usize::from(ult));
            assert_eq!(m.writes[0].0, 0x601);
            assert_eq!(m.writes[1].0, 0x603);
            assert_eq!((m.get(0x603) >> 50) & 1, u64::from(!ult));
            assert_eq!(m.get(0x601) & 1, 1);
            if ult {
                assert_eq!(m.writes[2], (0x636, ULT_FAST_RAMP_DEFAULT));
            }
        }
    }
}
