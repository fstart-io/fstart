//! Downcore and SMT-off policy (CCX_SP3 §3.6, decompiler-only).
//!
//! Requests are written to SMU::PWR::SOFT_DOWNCORE / THREAD_CONFIGURATION
//! of each die (Naples) or CCD (Rome/Milan) and committed with SMU message
//! 1; they take effect after a warm reset, which the platform performs.
//! Going back to the full topology needs a cold reset (Milan also sets CMOS
//! 0x72/0x73 index 0x0d bit 1, "discard memory context"); that bookkeeping
//! is the platform's.
//!
//! AGESA's validity rules are kept: a unit that already has fewer than 8
//! cores is left alone ("already down cored"), as is a request equal to the
//! current topology, and invalid modes are rejected (it logs event
//! 0x8000102 and continues; here the caller gets the error).
//!
//! Milan's donor goes through SMU services (+0x8 downcore base, +0x3c SMT,
//! +0xc/+0x14 1-CCD modes) that are not decoded; this uses the CCD
//! SMU::PWR registers as on Rome (Oxide's public `ccd.h` lists them for
//! Zen 3) and does not offer the 1-CCD modes.

use fstart_pci::ConfigRegionAccess;
use heapless::Vec;
use tock_registers::LocalRegisterCopy;

use super::release::{APPLY_KEY, MSG_APPLY, SmuPwr, ThreadCfg};
use crate::smn::{Smn, SmnAddr};
use crate::smu::{SmuError, SmuMessages, send1};
use crate::soc::Generation;

/// Enabled cores per complex (Milan has one 8-core complex: `ccx1 == 0`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoreMode {
    pub ccx0: u8,
    pub ccx1: u8,
}

/// Enabled CCDs per socket (Rome/Milan).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CcdMode {
    Two,
    Four,
    Six,
}

impl CcdMode {
    /// Physical CCDs to turn off (Rome PCD 0xa / Milan PCD 0x15d masks).
    pub const fn disabled(self) -> u8 {
        match self {
            Self::Two => 0xeb,
            Self::Four => 0xaa,
            Self::Six => 0x28,
        }
    }
}

/// The downcore/SMT request. Default: full topology, SMT on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DowncorePolicy {
    pub ccds: Option<CcdMode>,
    pub cores: Option<CoreMode>,
    pub smt: bool,
}

impl Default for DowncorePolicy {
    fn default() -> Self {
        Self {
            ccds: None,
            cores: None,
            smt: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DowncoreError {
    /// The core mode does not exist for this generation or package.
    CoreMode(CoreMode),
    /// CCD modes exist on Rome/Milan only.
    CcdMode,
    /// More than [`MAX_UNITS`] units.
    TooManyUnits,
    Smu(SmuError),
}

impl CoreMode {
    /// The SOFT_DOWNCORE value: disabled-core bits.
    pub fn discore(self, generation: Generation) -> Result<u8, DowncoreError> {
        let ok = match generation {
            // ZP PCD 0xa modes 1..7.
            Generation::Naples => matches!(
                (self.ccx0, self.ccx1),
                (1, 0) | (1, 1) | (2, 0) | (3, 0) | (2, 2) | (4, 0) | (3, 3)
            ),
            // SSP PCD 0xb: 2+0, 3+0, 4+0 are rejected for SP3 (PkgType 4).
            Generation::Rome => matches!((self.ccx0, self.ccx1), (1, 1) | (2, 2) | (3, 3)),
            // GN PCD 0x167: 1..7 of 8 cores (modes 1, 3, 4, 6, 8, 9, 10).
            Generation::Milan => self.ccx1 == 0 && (1..=7).contains(&self.ccx0),
        };
        if !ok {
            return Err(DowncoreError::CoreMode(self));
        }
        let enabled = |n: u8| (1u16 << n) - 1;
        let on = match generation {
            Generation::Milan => enabled(self.ccx0),
            _ => enabled(self.ccx0) | enabled(self.ccx1) << 4,
        };
        Ok(!(on as u8))
    }

    /// Milan with xAPIC forced on 256 threads ([`super::x2apic`]).
    pub const SEVEN_PER_CCD: Self = Self { ccx0: 7, ccx1: 0 };
}

/// One die (Naples) or CCD (Rome/Milan) as found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unit {
    pub socket: u8,
    /// Naples die; 0 on Rome/Milan.
    pub die: u8,
    /// Physical CCD (Rome/Milan); 0 on Naples.
    pub ccd: u8,
    /// Current THREAD_CONFIGURATION.
    pub thread_cfg: u32,
}

impl Unit {
    pub const fn regs(&self, generation: Generation) -> SmuPwr {
        match generation {
            Generation::Naples => SmuPwr::NAPLES,
            _ => SmuPwr::ccd(self.ccd),
        }
    }
}

/// One register update: `(v & !clear) | set` on the unit's SMN.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Write {
    pub socket: u8,
    pub die: u8,
    pub addr: SmnAddr,
    pub clear: u32,
    pub set: u32,
}

/// Most units: 2 sockets × 8 CCDs.
pub const MAX_UNITS: usize = 16;

/// The register writes for `policy`; empty if nothing changes.
pub fn plan(
    generation: Generation,
    policy: &DowncorePolicy,
    units: &[Unit],
) -> Result<Vec<Write, { 2 * MAX_UNITS }>, DowncoreError> {
    if policy.ccds.is_some() && generation == Generation::Naples {
        return Err(DowncoreError::CcdMode);
    }
    if units.len() > MAX_UNITS {
        return Err(DowncoreError::TooManyUnits);
    }
    let discore = policy.cores.map(|m| m.discore(generation)).transpose()?;
    let off = policy.ccds.map_or(0, CcdMode::disabled);
    let writes = units.iter().flat_map(|u| {
        let cfg = LocalRegisterCopy::<u32, ThreadCfg::Register>::new(u.thread_cfg);
        let regs = u.regs(generation);
        let write = |addr, clear, set| Write {
            socket: u.socket,
            die: u.die,
            addr,
            clear,
            set,
        };
        let complexes = cfg.read(ThreadCfg::COMPLEX_COUNT) as u8 + 1;
        let cores = cfg.read(ThreadCfg::CORE_COUNT) as u8 + 1;
        let current = CoreMode {
            ccx0: cores,
            ccx1: if complexes > 1 { cores } else { 0 },
        };
        let full = complexes * cores >= 8;
        let ccd_off = off & (1 << u.ccd) != 0 && generation != Generation::Naples;
        let downcore = if ccd_off {
            Some(write(regs.soft_downcore, 0xff, 0xff))
        } else {
            discore
                .filter(|_| full && policy.cores != Some(current))
                .map(|d| write(regs.soft_downcore, 0xff, d as u32))
        };
        let smt_on = !cfg.is_set(ThreadCfg::SMT_MODE);
        let smt = (!policy.smt && smt_on && !ccd_off)
            .then(|| write(regs.thread_cfg, 0, ThreadCfg::SMT_DISABLE::SET.value));
        downcore.into_iter().chain(smt)
    });
    Ok(writes.collect())
}

/// Write `writes` (all on this SMN) and commit them with SMU message 1.
pub fn commit<P, M>(smn: &Smn<P>, smu: &M, writes: &[Write]) -> Result<(), DowncoreError>
where
    P: ConfigRegionAccess,
    M: SmuMessages + ?Sized,
{
    if writes.is_empty() {
        return Ok(());
    }
    // SAFETY: the BSP owns the SMN pair during boot; these registers only
    // take effect at the next reset.
    writes
        .iter()
        .for_each(|w| unsafe { smn.modify32(w.addr, w.clear, w.set) });
    send1(smu, MSG_APPLY, APPLY_KEY).map_err(DowncoreError::Smu)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use Generation::*;

    #[test]
    fn core_masks() {
        let m = |a, b| CoreMode { ccx0: a, ccx1: b };
        let naples: std::vec::Vec<_> = [(1, 0), (1, 1), (2, 0), (3, 0), (2, 2), (4, 0), (3, 3)]
            .into_iter()
            .map(|(a, b)| m(a, b).discore(Naples).unwrap())
            .collect();
        assert_eq!(naples, [0xfe, 0xee, 0xfc, 0xf8, 0xcc, 0xf0, 0x88]);
        assert!(m(2, 0).discore(Rome).is_err(), "not on SP3");
        assert_eq!(m(3, 3).discore(Rome), Ok(0x88));
        assert_eq!(CoreMode::SEVEN_PER_CCD.discore(Milan), Ok(0x80));
        assert_eq!(m(5, 0).discore(Milan), Ok(0xe0));
        assert!(m(8, 0).discore(Milan).is_err());
    }

    #[test]
    fn plan_rules() {
        // Milan CCDs 0 (8 cores, SMT), 2 (6 cores: already down cored).
        let units = [
            Unit {
                socket: 0,
                die: 0,
                ccd: 0,
                thread_cfg: 0x07,
            },
            Unit {
                socket: 0,
                die: 0,
                ccd: 2,
                thread_cfg: 0x05,
            },
        ];
        let policy = DowncorePolicy {
            cores: Some(CoreMode::SEVEN_PER_CCD),
            smt: false,
            ccds: None,
        };
        let w = plan(Milan, &policy, &units).unwrap();
        assert_eq!(w.len(), 3);
        assert_eq!((w[0].addr, w[0].set), (0x3008_1020, 0x80));
        assert_eq!((w[1].addr, w[1].set), (0x3008_101c, 0x200));
        assert_eq!(w[2].addr, 0x3008_101c + 2 * 0x0200_0000);
        // Nothing to do by default or when the request is the topology.
        assert!(
            plan(Milan, &DowncorePolicy::default(), &units)
                .unwrap()
                .is_empty()
        );
        let same = DowncorePolicy {
            cores: Some(CoreMode { ccx0: 4, ccx1: 4 }),
            ..Default::default()
        };
        let rome = [Unit {
            socket: 0,
            die: 0,
            ccd: 3,
            thread_cfg: 0x13,
        }];
        assert!(plan(Rome, &same, &rome).is_err(), "4+4 is not a mode");
        let two = DowncorePolicy {
            ccds: Some(CcdMode::Two),
            ..Default::default()
        };
        let w = plan(Rome, &two, &rome).unwrap();
        assert_eq!((w[0].addr, w[0].set), (0x3008_1020 + 3 * 0x0200_0000, 0xff));
        assert_eq!(plan(Naples, &two, &[]), Err(DowncoreError::CcdMode));
    }
}
