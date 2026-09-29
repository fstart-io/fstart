//! Shared Core 2 / Pineview power-management MSR setup.
//!
//! The two model drivers supply only their model-specific enable bits. Both
//! use the same PMBASE-derived MWAIT I/O ports, C3 level and EIST lock order.

use super::msr_register::Msr;
use tock_registers::register_bitfields;

const MSR_PKG_CST_CONFIG_CONTROL: u32 = 0xe2;
const MSR_PMG_IO_BASE_ADDR: u32 = 0xe4;
const MSR_PMG_IO_CAPTURE_ADDR: u32 = 0xe7;
const IA32_MISC_ENABLE: u32 = 0x1a0;
const HIGHEST_CLEVEL: u64 = 3;

register_bitfields![u64,
    pub(super) CST [
        HIGHEST_CLEVEL OFFSET(0) NUMBITS(3) [],
        DYNAMIC_L2 OFFSET(3) NUMBITS(1) [],
        SINGLE_STOP_GRANT OFFSET(9) NUMBITS(1) [],
        IO_MWAIT_REDIRECT OFFSET(10) NUMBITS(1) [],
        DEEPER_SLEEP OFFSET(14) NUMBITS(1) [],
        LOCK OFFSET(15) NUMBITS(1) []
    ],
    PMG_IO_BASE [ PORT OFFSET(0) NUMBITS(16) [] ],
    PMG_IO_CAPTURE [ CSTATE_OFFSET OFFSET(16) NUMBITS(3) [] ],
    pub(super) MISC_ENABLE [
        TM1 OFFSET(3) NUMBITS(1) [],
        FERR_MUX OFFSET(10) NUMBITS(1) [],
        TM2 OFFSET(13) NUMBITS(1) [],
        EIST OFFSET(16) NUMBITS(1) [],
        PROCHOT OFFSET(17) NUMBITS(1) [],
        EIST_LOCK OFFSET(20) NUMBITS(1) [],
        C2E OFFSET(26) NUMBITS(1) [],
        C4E OFFSET(32) NUMBITS(1) [],
        HARD_C4E OFFSET(33) NUMBITS(1) [],
        EMTTM OFFSET(36) NUMBITS(1) []
    ]
];

fn c_state_bits(current: u64, extra: u64) -> u64 {
    let common = CST::LOCK::SET.value
        | CST::IO_MWAIT_REDIRECT::SET.value
        | CST::HIGHEST_CLEVEL.val(HIGHEST_CLEVEL).value;
    (current & !(CST::HIGHEST_CLEVEL.val(7).value | CST::SINGLE_STOP_GRANT::SET.value))
        | common
        | extra
}

/// Program shared C3/MWAIT state with the caller's model-specific CST bits.
///
/// # Safety
///
/// The current CPU must implement these MSRs and accept `extra`.
pub(super) unsafe fn configure_c_states(pmbase: u32, extra: u64) {
    let cst = Msr::<CST::Register>::new(MSR_PKG_CST_CONFIG_CONTROL);
    // SAFETY: guaranteed by the model driver.
    let current = unsafe { cst.read() }.get();
    unsafe {
        cst.write(c_state_bits(current, extra));
        Msr::<PMG_IO_BASE::Register>::new(MSR_PMG_IO_BASE_ADDR).write(
            PMG_IO_BASE::PORT
                .val(u64::from((pmbase + 4) & 0xffff))
                .value,
        );
        Msr::<PMG_IO_CAPTURE::Register>::new(MSR_PMG_IO_CAPTURE_ADDR).write(
            u64::from(pmbase + 4) | PMG_IO_CAPTURE::CSTATE_OFFSET.val(HIGHEST_CLEVEL - 2).value,
        );
    }
}

fn misc_enable_bits(current: u64, extra: u64) -> u64 {
    current
        | extra
        | MISC_ENABLE::TM1::SET.value
        | MISC_ENABLE::TM2::SET.value
        | MISC_ENABLE::PROCHOT::SET.value
        | MISC_ENABLE::FERR_MUX::SET.value
        | MISC_ENABLE::EIST::SET.value
}

/// Enable common thermal/EIST features, write them, then lock EIST.
///
/// # Safety
///
/// The current CPU must implement `IA32_MISC_ENABLE` and accept `extra`.
pub(super) unsafe fn configure_misc(extra: u64) {
    let misc = Msr::<MISC_ENABLE::Register>::new(IA32_MISC_ENABLE);
    let current = unsafe { misc.read() }.get();
    let enabled = misc_enable_bits(current, extra);
    unsafe {
        misc.write(enabled);
        misc.write(enabled | MISC_ENABLE::EIST_LOCK::SET.value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_specific_power_bits_preserve_the_original_register_values() {
        let core2_cst = CST::DEEPER_SLEEP::SET.value | CST::DYNAMIC_L2::SET.value;
        assert_eq!(c_state_bits(0, core2_cst), 0xc40b);
        assert_eq!(c_state_bits(0, 0), 0x8403); // Pineview
        assert_eq!(c_state_bits(1 << 9, 0) & (1 << 9), 0);

        let core2_misc = MISC_ENABLE::C2E::SET.value
            | MISC_ENABLE::C4E::SET.value
            | MISC_ENABLE::HARD_C4E::SET.value
            | MISC_ENABLE::EMTTM::SET.value;
        assert_eq!(misc_enable_bits(0, 0), 0x3_2408); // Pineview
        assert_eq!(misc_enable_bits(0, core2_misc), 0x13_0403_2408);
    }
}
