// SPDX-License-Identifier: MIT OR Apache-2.0
//! SMU topology policy/AP release.
//! Launching threads is not proof they reached the Rust AP rendezvous.
use super::{registers::Smn, smu};
use heapless::Vec;
const THREAD_ENABLE: u32 = 0x0005a818;
const THREAD_CONFIG: u32 = 0x0005a81c;
const DOWNCORE: u32 = 0x0005a860;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Downcore {
    Auto,
    One,
    Two,
    Three,
}
impl Downcore {
    fn request(self) -> Option<(u8, u32)> {
        match self {
            Self::Auto => None,
            Self::One => Some((1, 0xfe)),
            Self::Two => Some((2, 0xfc)),
            Self::Three => Some((3, 0xf8)),
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    pub downcore: Downcore,
    pub disable_smt: bool,
}
impl Policy {
    pub const DEFAULT: Self = Self {
        downcore: Downcore::Auto,
        disable_smt: false,
    };
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Topology {
    pub cores_per_complex: u8,
    pub complexes: u8,
    pub threads_per_core: u8,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Geometry,
    CannotEnableCores,
    ApCount,
    Unavailable(u32),
    Smu(smu::Error),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub enum Effect {
    Unchanged,
    ResetRequired,
}
impl Topology {
    fn decode(config: u32) -> Result<Self, Error> {
        let topology = Self {
            cores_per_complex: (config & 15) as u8 + 1,
            complexes: (config >> 4 & 15) as u8 + 1,
            threads_per_core: if config & (1 << 8) != 0 { 1 } else { 2 },
        };
        if topology.cores_per_complex > 4 || topology.complexes > 2 {
            return Err(Error::Geometry);
        }
        Ok(topology)
    }
    pub fn logical_threads(self) -> u8 {
        self.cores_per_complex * self.complexes * self.threads_per_core
    }
    /// ThreadEn bits use an eight-slot stride per complex, even with disabled
    /// cores/SMT. IDs are not a densely numbered count across complex boundaries.
    pub fn thread_slots(self) -> Result<Vec<u8, 16>, Error> {
        if !(1..=4).contains(&self.cores_per_complex)
            || !(1..=2).contains(&self.complexes)
            || !(1..=2).contains(&self.threads_per_core)
        {
            return Err(Error::Geometry);
        }
        let mut slots = Vec::new();
        for complex in 0..self.complexes {
            for thread in 0..self.cores_per_complex * self.threads_per_core {
                slots
                    .push(complex * 8 + thread)
                    .map_err(|_| Error::Geometry)?;
            }
        }
        Ok(slots)
    }
}
fn test(smn: &mut impl Smn, delay_us: &mut impl FnMut(u32)) -> Result<(), Error> {
    smu::request(smn, smu::TEST, [0xaa55aa55, 0, 0, 0, 0, 0], delay_us)
        .map(|_| ())
        .map_err(Error::Smu)
}
pub fn query(smn: &mut impl Smn, policy: Policy) -> Result<Topology, Error> {
    let mut topology = Topology::decode(smn.read(THREAD_CONFIG))?;
    if let Some((cores, _)) = policy.downcore.request() {
        topology.cores_per_complex = topology.cores_per_complex.min(cores);
    }
    if policy.disable_smt {
        topology.threads_per_core = 1;
    }
    Ok(topology)
}
/// Cold policy changes only take effect after a reset. Return that requirement
/// to the platform, which owns reset/failure handling; never proceed to APs.
pub fn apply(
    smn: &mut impl Smn,
    policy: Policy,
    delay_us: &mut impl FnMut(u32),
) -> Result<Effect, Error> {
    if policy == Policy::DEFAULT {
        return Ok(Effect::Unchanged);
    }
    let config = smn.read(THREAD_CONFIG);
    let topology = Topology::decode(config)?;
    let requested = policy.downcore.request();
    if requested.is_some_and(|(cores, _)| cores > topology.cores_per_complex) {
        return Err(Error::CannotEnableCores);
    }
    let downcore = requested.filter(|&(cores, _)| cores < topology.cores_per_complex);
    let disable_smt = policy.disable_smt && topology.threads_per_core > 1;
    if let Some((_, mask)) = downcore {
        smn.write(DOWNCORE, mask);
        test(smn, delay_us)?;
    }
    if disable_smt {
        smn.write(THREAD_CONFIG, config | (1 << 9));
        test(smn, delay_us)?;
    }
    Ok(if downcore.is_some() || disable_smt {
        Effect::ResetRequired
    } else {
        Effect::Unchanged
    })
}
/// S3 loses policy register programming; restoring it is a distinct operation,
/// not an atomic/cold-boot retry. Caller must hold the retained topology state.
pub fn restore(
    smn: &mut impl Smn,
    policy: Policy,
    delay_us: &mut impl FnMut(u32),
) -> Result<(), Error> {
    if let Some((_, mask)) = policy.downcore.request() {
        smn.write(DOWNCORE, mask);
        test(smn, delay_us)?;
    }
    if policy.disable_smt {
        smn.update(THREAD_CONFIG, 0, 1 << 9);
        test(smn, delay_us)?;
    }
    Ok(())
}
/// # Safety
/// Install resident, private-stack AP entry/rendezvous code before this call.
/// Every enabled thread (including already released ones) must have that
/// entry available. Keep it and its memory mappings alive until all APs have
/// joined; errors after a release require fail-stop/reset, not code reclamation.
pub unsafe fn launch(
    smn: &mut impl Smn,
    topology: Topology,
    ap_count: usize,
    delay_us: &mut impl FnMut(u32),
) -> Result<Vec<u8, 16>, Error> {
    let slots = topology.thread_slots()?;
    if ap_count >= slots.len() {
        return Err(Error::ApCount);
    }
    let mut released = Vec::new();
    for &bit in slots.iter().skip(1).take(ap_count) {
        let enabled = smn.read(THREAD_ENABLE);
        if enabled == u32::MAX {
            return Err(Error::Unavailable(THREAD_ENABLE));
        }
        if enabled & (1 << bit) == 0 {
            smn.write(THREAD_ENABLE, enabled | (1 << bit));
            test(smn, delay_us)?;
        }
        released.push(bit).map_err(|_| Error::ApCount)?;
    }
    Ok(released)
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::{collections::BTreeMap, vec::Vec};
    #[derive(Default)]
    struct Registers {
        regs: BTreeMap<u32, u32>,
        writes: Vec<(u32, u32)>,
    }
    impl Smn for Registers {
        fn read(&mut self, address: u32) -> u32 {
            if address == smu::RESPONSE {
                1
            } else {
                *self.regs.get(&address).unwrap_or(&0)
            }
        }
        fn write(&mut self, address: u32, value: u32) {
            self.regs.insert(address, value);
            self.writes.push((address, value));
        }
    }
    #[test]
    fn unavailable_thread_enable_is_not_an_already_released_cpu_set() {
        let mut registers = Registers::default();
        registers.regs.insert(THREAD_ENABLE, u32::MAX);
        let topology = Topology {
            cores_per_complex: 4,
            complexes: 1,
            threads_per_core: 2,
        };
        assert_eq!(
            unsafe { launch(&mut registers, topology, 7, &mut |_| {}) },
            Err(Error::Unavailable(THREAD_ENABLE))
        );
        assert!(registers.writes.is_empty());
    }

    #[test]
    fn masks_reset_requirement_and_complex_thread_stride() {
        let mut registers = Registers::default();
        registers.regs.insert(THREAD_CONFIG, 0x13); // 4 cores, 2 complexes, SMT
        let policy = Policy {
            downcore: Downcore::Two,
            disable_smt: true,
        };
        assert_eq!(
            apply(&mut registers, policy, &mut |_| {}).unwrap(),
            Effect::ResetRequired
        );
        assert_eq!(registers.regs[&DOWNCORE], 0xfc);
        assert_eq!(registers.regs[&THREAD_CONFIG], 0x213);
        let topology = query(&mut registers, policy).unwrap();
        assert_eq!(topology.thread_slots().unwrap().as_slice(), &[0, 1, 8, 9]);
        let released = unsafe { launch(&mut registers, topology, 3, &mut |_| {}) }.unwrap();
        assert_eq!(released.as_slice(), &[1, 8, 9]);
        assert_eq!(registers.regs[&THREAD_ENABLE], 0x302);
        registers.writes.clear();
        assert_eq!(
            unsafe { launch(&mut registers, topology, 4, &mut |_| {}) },
            Err(Error::ApCount)
        );
        assert!(registers.writes.is_empty());
        registers.regs.insert(THREAD_CONFIG, 0);
        assert_eq!(
            apply(&mut registers, policy, &mut |_| {}),
            Err(Error::CannotEnableCores)
        );
        assert!(registers.writes.is_empty());
    }
}
