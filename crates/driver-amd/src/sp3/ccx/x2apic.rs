//! APIC mode policy (CCX_SP3 §3.6).
//!
//! xAPIC physical destinations reach 255 APICs (ID 0xff is the broadcast),
//! so more than 254 threads need x2APIC. Only a 2P 64-core Rome or Milan
//! reaches 256. The mode reaches the hardware through the MSR tables: the
//! x2APIC records set APIC_BAR.x2ApicEn on every thread, the xAPIC records
//! hide CPUID x2APIC (C001_1004\[53\]); see [`super::table::PlatformFlags`].
//! Decide before the BSP runs its table and before any AP is released.

use crate::cpu::Silicon;

/// Most threads xAPIC can address individually.
pub const XAPIC_MAX_THREADS: usize = 254;

/// The APIC mode the tables are run with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApicMode {
    /// Milan's "compatibility": neither x2APIC table record applies; the
    /// CPU keeps advertising x2APIC and boots in xAPIC mode.
    Compatibility,
    XApic,
    X2Apic,
}

/// The requested mode (Milan PcdAmdApicMode; Rome's boolean
/// PcdAmdX2ApicMode is `X2Apic` or `Auto`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ApicModePolicy {
    Compatibility,
    XApic,
    X2Apic,
    /// Donor default (Milan 0xff; Rome FALSE behaves the same).
    #[default]
    Auto,
}

/// The resolved mode and whether xAPIC forces a downcore.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApicDecision {
    pub mode: ApicMode,
    /// Milan with xAPIC forced and too many threads: downcore every CCD to
    /// 7 cores (core mode 10, SOFT_DOWNCORE 0x80) so the IDs fit, then reset.
    pub downcore_to_7: bool,
}

/// Resolve the APIC mode for `threads` enabled threads in the system.
pub const fn decide(generation: Silicon, policy: ApicModePolicy, threads: usize) -> ApicDecision {
    let many = threads > XAPIC_MAX_THREADS;
    const fn mode(mode: ApicMode) -> ApicDecision {
        ApicDecision {
            mode,
            downcore_to_7: false,
        }
    }
    match (generation, policy) {
        // At most 2 × 32 cores × 2 threads.
        (Silicon::Zp, _) => mode(ApicMode::XApic),
        // SSP PEI: x2APIC PCD forced TRUE above 254 threads.
        (Silicon::Ssp, ApicModePolicy::X2Apic) => mode(ApicMode::X2Apic),
        (Silicon::Ssp, _) if many => mode(ApicMode::X2Apic),
        (Silicon::Ssp, _) => mode(ApicMode::XApic),
        // GN PEI `ffcfbacd` (instruction-checked).
        (Silicon::Gn, ApicModePolicy::X2Apic) => mode(ApicMode::X2Apic),
        (Silicon::Gn, ApicModePolicy::XApic) => ApicDecision {
            mode: ApicMode::XApic,
            downcore_to_7: many,
        },
        (Silicon::Gn, ApicModePolicy::Compatibility) if many => mode(ApicMode::Compatibility),
        (Silicon::Gn, ApicModePolicy::Compatibility) => mode(ApicMode::XApic),
        (Silicon::Gn, ApicModePolicy::Auto) if many => mode(ApicMode::X2Apic),
        (Silicon::Gn, ApicModePolicy::Auto) => mode(ApicMode::XApic),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ApicModePolicy::*;
    use Silicon::*;

    #[test]
    fn rome_forces_x2apic_above_254() {
        assert_eq!(decide(Ssp, Auto, 254).mode, ApicMode::XApic);
        assert_eq!(decide(Ssp, Auto, 256).mode, ApicMode::X2Apic);
        assert_eq!(decide(Ssp, X2Apic, 16).mode, ApicMode::X2Apic);
        assert_eq!(decide(Zp, X2Apic, 128).mode, ApicMode::XApic);
    }

    #[test]
    fn milan_policy() {
        assert_eq!(decide(Gn, Auto, 128).mode, ApicMode::XApic);
        assert_eq!(decide(Gn, Auto, 256).mode, ApicMode::X2Apic);
        assert_eq!(decide(Gn, Compatibility, 128).mode, ApicMode::XApic);
        assert_eq!(decide(Gn, Compatibility, 256).mode, ApicMode::Compatibility);
        let forced = decide(Gn, XApic, 256);
        assert_eq!(forced.mode, ApicMode::XApic);
        assert!(forced.downcore_to_7);
        assert!(!decide(Gn, XApic, 254).downcore_to_7);
    }
}
