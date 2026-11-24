// SPDX-License-Identifier: MIT OR Apache-2.0
//! Memory-controller CPU identification, from native/cpu.c.
//! Unlike the source fallback, unknown steppings (including Coffee Lake and
//! Amber Lake sharing these model numbers) are rejected, not assigned KBL PHY
//! settings. This is not an MP/SMM CPU driver.

use super::Error;

pub use fstart_arch::x86::cpu::intel::skylake::identification::{Family, Package, Stepping};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cpu {
    pub family: Family,
    pub package: Package,
    pub stepping: Stepping,
    pub signature: u32,
}
impl Cpu {
    pub fn from_signature(signature: u32) -> Result<Self, Error> {
        let id = fstart_arch::x86::cpu::intel::skylake::identification::identify(signature)
            .ok_or(Error::UnsupportedCpu)?;
        let (family, package, stepping) = (id.family, id.package, id.stepping);
        Ok(Self {
            family,
            package,
            stepping,
            signature,
        })
    }
    pub const fn model(self) -> u32 {
        self.signature & 0x0fff_0ff0
    }
    pub const fn stepping_id(self) -> u8 {
        (self.signature & 0xf) as u8
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_cpu_model_stepping_mapping() {
        assert_eq!(
            Cpu::from_signature(0x506e3).unwrap().family,
            Family::Skylake
        );
        assert_eq!(
            Cpu::from_signature(0x506e8).unwrap().family,
            Family::KabyLake
        );
        assert_eq!(
            Cpu::from_signature(0x906ea).unwrap().stepping,
            Stepping::KblN0
        );
        assert_eq!(
            Cpu::from_signature(0x806e9).unwrap().package,
            Package::UltUlx
        );
        for signature in [0x906eb, 0x906ec, 0x806eb, 0x306a9, 0x506ef, 0] {
            assert_eq!(Cpu::from_signature(signature), Err(Error::UnsupportedCpu));
        }
    }
}
