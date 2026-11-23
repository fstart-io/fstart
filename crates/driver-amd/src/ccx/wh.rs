// SPDX-License-Identifier: MIT OR Apache-2.0
//! WH (Zen 6, Family 1Ah models 50h–5Fh) CCX primitives: dual-IOD indexing,
//! SMN `ThreadEnable` AP release with the documented silicon erratum, and
//! CPUID leaf `0x8000_0026` APIC ID calculation accumulating prior dies' CCDs.

use crate::smn::{SmnAccess, SmnReg};
use tock_registers::interfaces::{Readable, Writeable};
use tock_registers::register_bitfields;

/// Family 1Ah extended family + base family (`0x0F + 0x0B = 0x1A`).
pub const FAMILY_1AH: u8 = 0x1a;
/// WH model range (models `0x50..=0x5F`, CPUID EAX `0x00B5_0Fxx`).
pub const MODEL_MIN: u8 = 0x50;
pub const MODEL_MAX: u8 = 0x5f;
/// Maximum logical IODs per system (2 sockets × 2 IODs).
pub const MAX_IODS: u8 = 4;

/// SMN `ThreadEnable` low word (`0x2076_0020`, 32-bit, threads 0..=31 of a CCD).
pub const THREAD_ENABLE_LO_BASE: u32 = 0x2076_0020;
/// SMN `ThreadEnable` high word (`0x2076_0024`, 32-bit, threads 32..=63 of a CCD).
pub const THREAD_ENABLE_HI_BASE: u32 = 0x2076_0024;

register_bitfields![u32,
    /// SMN `(phys_ccd << 24) | 0x2076_0020` / `0x2076_0024` (32-bit):
    /// per-CCD AP thread release bitmap (bit `t` releases thread `t`).
    pub ThreadEnable [
        MASK OFFSET(0) NUMBITS(32) []
    ]
];

/// WH core revision decoded from CPUID Fn0000_0001_EAX.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Revision {
    pub signature: u32,
    pub model: u8,
    pub stepping: u8,
}

impl Revision {
    /// Decode CPUID Fn0000_0001_EAX if it belongs to Family 1Ah models 50h–5Fh.
    pub const fn from_signature(signature: u32) -> Option<Self> {
        let base_family = ((signature >> 8) & 0x0f) as u8;
        let ext_family = ((signature >> 20) & 0xff) as u8;
        let family = if base_family == 0x0f {
            base_family.saturating_add(ext_family)
        } else {
            base_family
        };
        let model = ((((signature >> 16) & 0x0f) << 4) | ((signature >> 4) & 0x0f)) as u8;
        let stepping = (signature & 0x0f) as u8;
        if family == FAMILY_1AH && model >= MODEL_MIN && model <= MODEL_MAX {
            Some(Self {
                signature,
                model,
                stepping,
            })
        } else {
            None
        }
    }

    /// True for steppings that require an extra read-write cycle on the low
    /// `ThreadEnable` word after writing the high word (Family 1Ah model 50h
    /// stepping 0h or Bh, CPUID `0x00B5_0F00` / `0x00B5_0F0B`).
    pub const fn needs_thread_enable_low_kick(self) -> bool {
        self.model == 0x50 && matches!(self.stepping, 0x0 | 0xb)
    }
}

/// Linear IOD index `(socket?2:0) + (die?1:0)` for dual-IOD sockets.
pub const fn iod_index(socket: u8, die: u8) -> Option<u8> {
    if socket >= 2 || die >= 2 {
        return None;
    }
    Some((socket << 1) | die)
}

/// Low and high `ThreadEnable` SMN registers for physical CCD `phys_ccd` (`0..=15`).
pub const fn thread_enable_regs(
    phys_ccd: u8,
) -> Option<(
    SmnReg<ThreadEnable::Register>,
    SmnReg<ThreadEnable::Register>,
)> {
    if phys_ccd >= 16 {
        return None;
    }
    let ccd = (phys_ccd as u32) << 24;
    Some((
        SmnReg::new(ccd | THREAD_ENABLE_LO_BASE),
        SmnReg::new(ccd | THREAD_ENABLE_HI_BASE),
    ))
}

/// Release threads in 64-bit `mask` on `phys_ccd` through `smn`, applying the
/// extra low-word read/write erratum when `revision.needs_thread_enable_low_kick()`
/// is set and upper threads (`mask >> 32 != 0`) are enabled.
pub fn enable_threads(
    smn: &impl SmnAccess,
    revision: Revision,
    phys_ccd: u8,
    mask: u64,
) -> Option<()> {
    let (lo_reg, hi_reg) = thread_enable_regs(phys_ccd)?;
    let lo = mask as u32;
    let hi = (mask >> 32) as u32;
    if lo != 0 {
        let cur = smn.reg(lo_reg).get();
        smn.reg(lo_reg).set(cur | lo);
    }
    if hi != 0 {
        let cur = smn.reg(hi_reg).get();
        smn.reg(hi_reg).set(cur | hi);
        if revision.needs_thread_enable_low_kick() {
            let cur_lo = smn.reg(lo_reg).get();
            smn.reg(lo_reg).set(cur_lo);
        }
    }
    Some(())
}

/// Shift widths from CPUID leaf `0x8000_0026` subleaves 0..=3 (`EAX[4:0]`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TopologyShifts {
    /// Core shift (level type 1 / thread): bit position of logical core ID.
    pub core_shift: u8,
    /// Complex shift (level type 2 / core): bit position of logical complex ID.
    pub complex_shift: u8,
    /// CCD shift (level type 3 / complex): bit position of socket-wide CCD ID.
    pub ccd_shift: u8,
    /// Socket shift (level type 4 / die/CCD): bit position of socket ID.
    pub socket_shift: u8,
}

impl TopologyShifts {
    /// Decode CPUID leaf `0x8000_0026` `(eax, ecx)` pairs by level type
    /// (`(ecx >> 8) & 0xff`: `1`=thread, `2`=core, `3`=complex, `4`=socket).
    pub const fn from_cpuid_subleaves(subleaves: &[(u32, u32)]) -> Option<Self> {
        let mut core_shift = None;
        let mut complex_shift = None;
        let mut ccd_shift = None;
        let mut socket_shift = None;
        let mut i = 0;
        while i < subleaves.len() {
            let (eax, ecx) = subleaves[i];
            let level_type = (ecx >> 8) & 0xff;
            let shift = (eax & 0x1f) as u8;
            match level_type {
                0 => break,
                1 => core_shift = Some(shift),
                2 => complex_shift = Some(shift),
                3 => ccd_shift = Some(shift),
                4 => socket_shift = Some(shift),
                _ => {}
            }
            i += 1;
        }
        let (Some(core_shift), Some(complex_shift), Some(ccd_shift), Some(socket_shift)) =
            (core_shift, complex_shift, ccd_shift, socket_shift)
        else {
            return None;
        };
        if core_shift > complex_shift
            || complex_shift > ccd_shift
            || ccd_shift > socket_shift
            || socket_shift >= 32
        {
            return None;
        }
        Some(Self {
            core_shift,
            complex_shift,
            ccd_shift,
            socket_shift,
        })
    }
}

const fn fits_bits(value: u32, bits: u8) -> bool {
    if bits >= 32 {
        true
    } else {
        (value >> bits) == 0
    }
}

/// Compute the x2APIC ID for a thread on a dual-IOD socket, accumulating
/// `prior_dies_ccds` (the CCD count of earlier dies on the same socket) and
/// validating field widths against `shifts`.
#[allow(clippy::too_many_arguments)]
pub const fn apic_id(
    shifts: TopologyShifts,
    socket: u32,
    prior_dies_ccds: u32,
    logical_ccd: u32,
    logical_complex: u32,
    logical_core: u32,
    thread: u32,
    smt: bool,
) -> Option<u32> {
    if shifts.core_shift > shifts.complex_shift
        || shifts.complex_shift > shifts.ccd_shift
        || shifts.ccd_shift > shifts.socket_shift
        || shifts.socket_shift >= 32
    {
        return None;
    }
    if (!smt && thread != 0) || !fits_bits(thread, shifts.core_shift) {
        return None;
    }
    if !fits_bits(logical_core, shifts.complex_shift - shifts.core_shift)
        || !fits_bits(logical_complex, shifts.ccd_shift - shifts.complex_shift)
    {
        return None;
    }
    let Some(socket_ccd) = prior_dies_ccds.checked_add(logical_ccd) else {
        return None;
    };
    if !fits_bits(socket_ccd, shifts.socket_shift - shifts.ccd_shift)
        || !fits_bits(socket, 32 - shifts.socket_shift)
    {
        return None;
    }
    Some(
        (socket << shifts.socket_shift)
            | (socket_ccd << shifts.ccd_shift)
            | (logical_complex << shifts.complex_shift)
            | (logical_core << shifts.core_shift)
            | thread,
    )
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use crate::smn::testing::{SmnModel, SmnTest};
    use std::vec::Vec;

    #[derive(Default)]
    struct TraceSmn {
        regs: std::collections::BTreeMap<u32, u32>,
        ops: Vec<(char, u32, u32)>,
    }

    impl SmnModel for TraceSmn {
        fn read(&mut self, addr: u32) -> u32 {
            let val = *self.regs.get(&addr).unwrap_or(&0);
            self.ops.push(('R', addr, val));
            val
        }
        fn write(&mut self, addr: u32, value: u32) {
            self.regs.insert(addr, value);
            self.ops.push(('W', addr, value));
        }
    }

    #[test]
    fn revision_detects_wh_and_erratum() {
        let a0 = Revision::from_signature(0x00b5_0f00).unwrap();
        assert_eq!((a0.model, a0.stepping), (0x50, 0));
        assert!(a0.needs_thread_enable_low_kick());

        let b0 = Revision::from_signature(0x00b5_0f0b).unwrap();
        assert!(b0.needs_thread_enable_low_kick());

        let c0 = Revision::from_signature(0x00b5_0f21).unwrap();
        assert!(!c0.needs_thread_enable_low_kick());

        assert!(Revision::from_signature(0x00a1_0f11).is_none());
    }

    #[test]
    fn thread_enable_enforces_low_high_low_erratum_write_order() {
        let lo = 0x0300_0000 | THREAD_ENABLE_LO_BASE;
        let hi = 0x0300_0000 | THREAD_ENABLE_HI_BASE;
        let mask = 0x0000_000f_ffff_ffffu64;

        let erratum = SmnTest::new(TraceSmn::default());
        let a0 = Revision::from_signature(0x00b5_0f00).unwrap();
        enable_threads(&erratum.smn(), a0, 3, mask).unwrap();
        let writes: Vec<(u32, u32)> = erratum
            .model()
            .ops
            .iter()
            .filter(|(op, _, _)| *op == 'W')
            .map(|(_, a, v)| (*a, *v))
            .collect();
        assert_eq!(
            writes,
            std::vec![(lo, 0xffff_ffff), (hi, 0x0000_000f), (lo, 0xffff_ffff)]
        );

        let clean = SmnTest::new(TraceSmn::default());
        let c0 = Revision::from_signature(0x00b5_0f21).unwrap();
        enable_threads(&clean.smn(), c0, 3, mask).unwrap();
        let writes: Vec<(u32, u32)> = clean
            .model()
            .ops
            .iter()
            .filter(|(op, _, _)| *op == 'W')
            .map(|(_, a, v)| (*a, *v))
            .collect();
        assert_eq!(writes, std::vec![(lo, 0xffff_ffff), (hi, 0x0000_000f)]);
        assert!(thread_enable_regs(16).is_none());
    }

    #[test]
    fn apic_id_decodes_cpuid_subleaves_complex_and_smt() {
        let shifts = TopologyShifts::from_cpuid_subleaves(&[
            (1, 1 << 8),
            (5, 2 << 8),
            (6, 3 << 8),
            (10, 4 << 8),
        ])
        .unwrap();
        // Socket 0, die 1 (after 4 CCDs on die 0), logical CCD 1 -> socket CCD 5,
        // complex 1, core 2, SMT thread 1.
        assert_eq!(
            apic_id(shifts, 0, 4, 1, 1, 2, 1, true),
            Some((5 << 6) | (1 << 5) | (2 << 1) | 1)
        );
        // Rejects non-zero thread when SMT is disabled or complex overflows its field.
        assert_eq!(apic_id(shifts, 0, 4, 1, 1, 2, 1, false), None);
        assert_eq!(apic_id(shifts, 0, 4, 1, 2, 2, 0, false), None);
        assert_eq!(
            apic_id(shifts, 1, 4, 0, 0, 0, 0, false),
            Some((1 << 10) | (4 << 6))
        );
    }
}
