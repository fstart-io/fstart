// SPDX-License-Identifier: MIT OR Apache-2.0
//! Zen 4 CCX "program tables at reset" (AGESA CcxProgramTablesAtReset),
//! applied on every thread before it runs OS code. [`RESET`] is generated
//! from TYAN S8040GM2NE-NFX V0.02 AmdCcxZen4Pei (see [`reset`]); the engine
//! at `ffa27b03` runs scopes in order, per-thread then first thread of a
//! core then of a complex. Opcode guards: 4 = CPU logical family/revision
//! bits, 5 = core logical ID, 6 = platform flags, 7 = both. Bits stay raw.
use crate::table;

mod reset;
pub use reset::RESET;

/// Which threads run a record, and the guards it carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct When {
    pub scope: Scope,
    /// CPU logical family and revision mask (overlap passes).
    pub cpu: Option<(u32, u32)>,
    /// Core logical family and revision mask.
    pub core: Option<(u32, u32)>,
    /// Platform-flag bits (any passes).
    pub platform: Option<u32>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Thread,
    Core,
    Complex,
}

/// CPU logical IDs (family bit, revision bit) from the Rs SoC xlat table of
/// NFX AmdSocSp5RsPei (`ffa6fb50`, revision rows `ffa6fb0c`/`ffa6fb20`):
/// family 19h extended model 1 (Genoa) is logical family `0x1000`, base
/// model 1 steppings 0/1/2 revision `0x10`/`0x20`/`0x40`; extended model Ah
/// (Siena) is `0x8000`, steppings 0/1 revision `0x1`/`0x2`. AGESA maps an
/// unlisted Siena stepping (A2) to its newest row, and every family-`0x8000`
/// record covers both revisions. Other CPUs have no known logical ID.
pub const fn logical_id(signature: u32) -> Option<(u32, u32)> {
    let family = ((signature >> 8) & 0xf) + ((signature >> 20) & 0xff);
    let extended_model = (signature >> 16) & 0xf;
    let model = (signature >> 4) & 0xf;
    let stepping = signature & 0xf;
    if family != 0x19 {
        return None;
    }
    match (extended_model, model, stepping) {
        (0x1, 0x1, 0) => Some((0x1000, 0x10)),
        (0x1, 0x1, 1) => Some((0x1000, 0x20)),
        (0x1, 0x1, 2) => Some((0x1000, 0x40)),
        (0xa, 0x0, 0) => Some((siena::CPU_FAMILY, 0x1)),
        (0xa, 0x0, _) => Some((siena::CPU_FAMILY, siena::CPU_REVISION)),
        _ => None,
    }
}

pub mod siena {
    pub const CPU_FAMILY: u32 = 0x8000;
    pub const CPU_REVISION: u32 = 0x2;
}

/// What the executing thread is: its CPU (and, if known, core) logical ID,
/// the platform flags and its position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Thread {
    pub cpu: (u32, u32),
    /// Unknown core logical IDs skip core-guarded records.
    pub core: Option<(u32, u32)>,
    /// Unknown flags (`None`) skip platform-guarded records.
    pub platform: Option<u32>,
    pub first_of_core: bool,
    pub first_of_complex: bool,
}

impl When {
    pub const fn applies(&self, thread: &Thread) -> bool {
        const fn overlaps(guard: Option<(u32, u32)>, id: Option<(u32, u32)>) -> bool {
            match (guard, id) {
                (None, _) => true,
                (Some(g), Some(id)) => g.0 & id.0 != 0 && g.1 & id.1 != 0,
                (Some(_), None) => false,
            }
        }
        let platform = match (self.platform, thread.platform) {
            (None, _) => true,
            (Some(want), Some(have)) => want & have != 0,
            (Some(_), None) => false,
        };
        overlaps(self.cpu, Some(thread.cpu)) && overlaps(self.core, thread.core) && platform
    }
}

impl Thread {
    const fn in_scope(&self, scope: Scope) -> bool {
        match scope {
            Scope::Thread => true,
            Scope::Core => self.first_of_core,
            Scope::Complex => self.first_of_complex,
        }
    }
}

/// Run the reset table on the executing thread. As the vendor engine, the
/// first scope this thread is not in ends the run.
pub fn apply_reset(cpu: &impl fstart_arch::x86::cpu::msr::MsrAccess, thread: &Thread) {
    table::apply_when(
        RESET
            .iter()
            .copied()
            .take_while(|record| thread.in_scope(record.when.scope)),
        |when| when.applies(thread),
        |msr| cpu.reg(msr),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use fstart_arch::x86::cpu::msr::testing::MsrSpace;

    #[test]
    fn siena_records_follow_family_scope_and_unknown_guards() {
        let thread = Thread {
            cpu: (siena::CPU_FAMILY, siena::CPU_REVISION),
            core: None,
            platform: None,
            first_of_core: true,
            first_of_complex: false,
        };
        let cpu = MsrSpace::new([(0xc001_0402, 0xff), (0xc001_1022, 0), (0x1b, 0xfee0_0900)]);
        apply_reset(&cpu, &thread);
        let regs = cpu.into_inner();
        // Generic set, then the family-0x8000 clear, in vendor order.
        assert_eq!(regs.get(0xc001_0402), 0xf7);
        assert_eq!(regs.get(0xc001_1000) >> 24, 0x33);
        // Core scope ran (DC_CFG), complex scope did not.
        assert_eq!(regs.get(0xc001_1022), 0x8680_0000_0000_0000);
        assert!(!regs.written(0xc001_1093));
        // Platform-guarded x2APIC enable skipped without known flags.
        assert_eq!(regs.get(0x1b), 0xfee0_0900);
        // Genoa-only records do not apply.
        assert!(!regs.written(0xc001_10de));
    }

    #[test]
    fn logical_ids_follow_the_vendor_xlat() {
        assert_eq!(logical_id(0x00a1_0f10), Some((0x1000, 0x10)));
        assert_eq!(logical_id(0x00a1_0f11), Some((0x1000, 0x20)));
        assert_eq!(logical_id(0x00aa_0f00), Some((0x8000, 0x1)));
        assert_eq!(logical_id(0x00aa_0f02), Some((0x8000, 0x2)));
        // Milan, Genoa A0 (base model 0) and Turin have no known row.
        for signature in [0x00a0_0f11, 0x00a1_0f01, 0x00b0_0f20, 0x0083_0f10] {
            assert_eq!(logical_id(signature), None);
        }
    }
}
