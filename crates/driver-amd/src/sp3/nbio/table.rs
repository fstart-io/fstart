//! The two NBIO register-table formats and their engines (NBIO_SMU §3.2).
//!
//! The tables themselves are generated from the contracts JSON
//! (`naples/tables.rs`, `sp3/tables.rs`); here are the record types and the
//! interpreters. Only the record forms the SP3 tables use exist: writes,
//! read-modify-writes and (Naples) whole-dword copies, all in SMN space.
//! AGESA's poll records do not occur in these tables.

use fstart_pci::ConfigRegionAccess;

use super::{Nbio, NbioPolicy};
use crate::sp3::smn::SmnAddr;
use crate::sp3::soc::Generation;

/// One register operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    /// Write the value.
    Write(u32),
    /// `(old & !clear) | set`.
    Rmw { clear: u32, set: u32 },
    /// Copy the dword at `from` to the record's address (Naples COPY).
    Copy { from: SmnAddr },
}

/// A Naples ("legacy" GNB bytecode) record.
///
/// A record with a non-zero `property` applies when it shares a bit with
/// the live property word ([`super::naples::property`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LegacyEntry {
    pub addr: SmnAddr,
    pub op: Op,
    pub property: u32,
}

impl LegacyEntry {
    pub const fn write(addr: SmnAddr, value: u32) -> Self {
        Self {
            addr,
            op: Op::Write(value),
            property: 0,
        }
    }

    pub const fn rmw(addr: SmnAddr, clear: u32, set: u32) -> Self {
        Self {
            addr,
            op: Op::Rmw { clear, set },
            property: 0,
        }
    }

    pub const fn copy(to: SmnAddr, from: SmnAddr) -> Self {
        Self {
            addr: to,
            op: Op::Copy { from },
            property: 0,
        }
    }

    pub const fn property(self, property: u32) -> Self {
        Self { property, ..self }
    }

    /// Whether the record applies under the live `property`.
    pub const fn applies(&self, property: u32) -> bool {
        self.property == 0 || self.property & property != 0
    }
}

/// Address rule of a Rome/Milan record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Scopes 0/1: `addr + (nbio << 20)`.
    Nbio,
    /// Milan scope 4: `addr + (nbio ? (nbio + 9) << 20 : 0)`.
    Sdpmux,
}

/// The policy knobs the Rome/Milan tables test. Each stands for one PCD per
/// generation (Rome / Milan token in the docs); [`NbioPolicy::knob`] returns
/// the value AGESA's PCD would hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Knob {
    /// 0xFD / 0xA0: IOHC, IOAGR, SDPMUX, IOAPIC clock gating.
    IohcClockGating,
    /// 0x102 / 0x8E.
    SstClockGating,
    /// 0x103 / 0x85: NBIF `MGCG_CTL_LCLK` bit 0.
    NbifClockGating,
    /// 0x104 / 0x8F: NBIF ALT (SYSHUB) clock gating.
    SyshubClockGating,
    /// 0x105 / 0x1AD: HD audio (NBIF1 F4; the tables address NBIO2).
    HdAudio,
    /// 0x106 / 0xB5: the PTDMA functions.
    Ptdma,
    /// 0x32 / 0x1B0.
    IommuEnable,
    /// 0xE (`== 1` enables) / 0x2C (`!= 1` enables).
    IommuXtGa,
    /// 0xF9 / 0xA1.
    IommuL1ClockGating,
    /// 0xFA / 0xA2.
    IommuL2ClockGating,
    /// 0xFB / 0x137: `L2_TW_CONTROL` bit 16.
    IommuL2TwBit16,
    /// Milan 0x110: PCIe core `0x70`/`0x74` feature set A.
    PcieCoreFeatureA,
    /// Milan 0x111: feature set B.
    PcieCoreFeatureB,
    /// Milan 0x136: NBIF TPH straps.
    NbifTph,
    /// Milan 0x194: `IOHC_FCTL` P2P mode (1 forward-non-matching, 2 forward-all).
    IohcP2p,
}

/// PCD comparison of a record (AGESA compare byte 0 / 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cmp {
    Eq,
    Ne,
}

/// Predicate of a Rome/Milan record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gate {
    Always,
    Knob { knob: Knob, cmp: Cmp, value: u32 },
}

/// A Rome/Milan (`GnbBlastTable`) record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    pub addr: SmnAddr,
    pub scope: Scope,
    pub op: Op,
    pub gate: Gate,
}

impl Entry {
    pub const fn write(addr: SmnAddr, value: u32) -> Self {
        Self {
            addr,
            scope: Scope::Nbio,
            op: Op::Write(value),
            gate: Gate::Always,
        }
    }

    pub const fn rmw(addr: SmnAddr, clear: u32, set: u32) -> Self {
        Self {
            addr,
            scope: Scope::Nbio,
            op: Op::Rmw { clear, set },
            gate: Gate::Always,
        }
    }

    pub const fn sdpmux(self) -> Self {
        Self {
            scope: Scope::Sdpmux,
            ..self
        }
    }

    pub const fn when(self, knob: Knob, cmp: Cmp, value: u32) -> Self {
        Self {
            gate: Gate::Knob { knob, cmp, value },
            ..self
        }
    }

    /// Whether the record applies on `generation` under `policy`.
    pub fn applies(&self, generation: Generation, policy: &NbioPolicy) -> bool {
        match self.gate {
            Gate::Always => true,
            Gate::Knob { knob, cmp, value } => {
                let live = policy.knob(generation, knob);
                match cmp {
                    Cmp::Eq => live == value,
                    Cmp::Ne => live != value,
                }
            }
        }
    }

    /// The SMN address for NBIO quadrant `quadrant`.
    pub const fn address(&self, quadrant: u8) -> SmnAddr {
        let q = quadrant as u32;
        match self.scope {
            Scope::Nbio => self.addr + (q << 20),
            Scope::Sdpmux if q == 0 => self.addr,
            Scope::Sdpmux => self.addr + ((q + 9) << 20),
        }
    }
}

impl<P: ConfigRegionAccess + Clone> Nbio<P> {
    fn apply(&self, addr: SmnAddr, op: Op) {
        match op {
            Op::Write(v) => self.write32(addr, v),
            Op::Rmw { clear, set } => self.modify32(addr, clear, set),
            Op::Copy { from } => {
                if let Some(v) = self.read32(from) {
                    self.write32(addr, v)
                }
            }
        }
    }

    /// Run a Naples table on this die under the live `property`.
    pub fn run_legacy(&self, table: &[LegacyEntry], property: u32) {
        table
            .iter()
            .filter(|e| e.applies(property))
            .for_each(|e| self.apply(e.addr, e.op));
    }

    /// Run a Rome/Milan table as quadrant `quadrant` through this NBIO's
    /// SMN. Per-NBIO tables pass [`Nbio::index`]; the socket tables pass 0
    /// (they carry absolute addresses for all four quadrants).
    pub fn run_table(&self, table: &[Entry], quadrant: u8, policy: &NbioPolicy) {
        table
            .iter()
            .filter(|e| e.applies(self.generation, policy))
            .for_each(|e| self.apply(e.address(quadrant), e.op));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sp3::nbio::tests::nbio;

    #[test]
    fn legacy_property_and_copy() {
        let (model, n) = nbio(Generation::Naples, 0, 0x20);
        model.0.borrow_mut().smn.insert((0x20, 0x13b1_007c), 0xabcd);
        let table = [
            LegacyEntry::rmw(0x100, 0xff, 0x12).property(0x40),
            LegacyEntry::write(0x104, 7).property(0x4),
            LegacyEntry::copy(0x108, 0x13b1_007c),
        ];
        n.run_legacy(&table, 0x200008C0);
        let s = model.0.borrow();
        assert_eq!(s.smn[&(0x20, 0x100)], 0x12);
        assert!(!s.smn.contains_key(&(0x20, 0x104)), "property 4 is off");
        assert_eq!(s.smn[&(0x20, 0x108)], 0xabcd);
    }

    #[test]
    fn modern_scope_and_gate() {
        let (model, n) = nbio(Generation::Milan, 2, 0x40);
        let off = NbioPolicy {
            ptdma: false,
            ..NbioPolicy::default()
        };
        let table = [
            Entry::write(0x13b1_0118, 1),
            Entry::write(0x0440_0400, 2).sdpmux(),
            Entry::write(0x1013_4400, 3).when(Knob::Ptdma, Cmp::Eq, 1),
        ];
        n.run_table(&table, n.index, &off);
        let s = model.0.borrow();
        assert_eq!(s.smn[&(0x40, 0x13d1_0118)], 1);
        assert_eq!(s.smn[&(0x40, 0x0440_0400 + (11 << 20))], 2);
        assert!(!s.smn.contains_key(&(0x40, 0x1033_4400)));
        assert_eq!(
            Entry::write(0x0440_0400, 0).sdpmux().address(0),
            0x0440_0400
        );
    }
}
