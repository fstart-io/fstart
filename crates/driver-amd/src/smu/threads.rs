// SPDX-License-Identifier: MIT OR Apache-2.0
//! The threads of a die, which its SMU holds in reset until software sets
//! their ThreadEn bit ([`launch`]); its ThreadConfig is the thread topology,
//! which a [`Policy`] can reduce for the next reset ([`apply`]).
use crate::{
    fam17h::{
        topology::{Die, Soc},
        tsc::DelayNs,
    },
    smn::Smn,
    smu::{self, DOWNCORE, THREAD_CONFIG, THREAD_ENABLE, ThreadConfig},
};
use fstart_pci::ConfigRegionAccess;
use tock_registers::{
    LocalRegisterCopy,
    interfaces::{ReadWriteable, Readable, Writeable},
};

/// Cores per complex the SMU keeps enabled at the next reset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Downcore {
    Auto,
    One,
    Two,
    Three,
}
impl Downcore {
    /// (cores per complex, DOWNCORE mask).
    fn request(self) -> Option<(u8, u32)> {
        match self {
            Self::Auto => None,
            Self::One => Some((1, 0xfe)),
            Self::Two => Some((2, 0xfc)),
            Self::Three => Some((3, 0xf8)),
        }
    }
}
/// The board's thread policy: fewer cores per complex, no SMT.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    pub downcore: Downcore,
    pub disable_smt: bool,
}
impl Policy {
    /// Every thread the part has.
    pub const DEFAULT: Self = Self {
        downcore: Downcore::Auto,
        disable_smt: false,
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// ThreadConfig doesn't fit the eight ThreadEn slots per complex.
    Geometry,
    /// The policy asks for more cores than the part has.
    CannotEnableCores,
    /// Fewer threads exist than APs were requested; the ones that exist
    /// were released.
    ApCount { started: usize, requested: usize },
    /// ThreadEn of a die reads all ones.
    Unavailable { die: u8 },
    /// A die's SMU didn't complete the TEST message.
    Smu { die: u8, error: smu::Error },
}
/// What [`apply`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub enum Effect {
    Unchanged,
    ResetRequired,
}

/// The thread topology of a die.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Threads {
    pub cores_per_complex: u8,
    pub complexes: u8,
    pub threads_per_core: u8,
}
impl Threads {
    fn decode(config: LocalRegisterCopy<u32, ThreadConfig::Register>) -> Result<Self, Error> {
        let threads = Self {
            cores_per_complex: config.read(ThreadConfig::CORE_COUNT) as u8 + 1,
            complexes: config.read(ThreadConfig::COMPLEX_COUNT) as u8 + 1,
            threads_per_core: if config.is_set(ThreadConfig::NOT_SMT) {
                1
            } else {
                2
            },
        };
        if threads.cores_per_complex > 4 || threads.complexes > 2 {
            return Err(Error::Geometry);
        }
        Ok(threads)
    }
    /// The threads of a die as `policy` leaves them.
    pub fn query(smn: &Smn<impl ConfigRegionAccess>, policy: Policy) -> Result<Self, Error> {
        let mut threads = Self::decode(smn.reg(THREAD_CONFIG).extract())?;
        if let Some((cores, _)) = policy.downcore.request() {
            threads.cores_per_complex = threads.cores_per_complex.min(cores);
        }
        if policy.disable_smt {
            threads.threads_per_core = 1;
        }
        Ok(threads)
    }
    pub const fn threads_per_complex(self) -> u8 {
        self.cores_per_complex * self.threads_per_core
    }
    pub const fn count(self) -> usize {
        self.complexes as usize * self.threads_per_complex() as usize
    }
    /// The ThreadEn bits of the threads: an eight-slot stride per complex,
    /// even with disabled cores or SMT off. Thread IDs are not dense across
    /// complex boundaries.
    pub fn slots(self) -> impl Iterator<Item = u8> {
        (0..self.complexes).flat_map(move |complex| {
            (0..self.threads_per_complex()).map(move |thread| complex * 8 + thread)
        })
    }
}

fn test<S: Soc, P: ConfigRegionAccess + Copy>(
    die: &Die<S, P>,
    delay: &mut impl DelayNs,
) -> Result<(), Error> {
    S::SMU.test(&die.smn(), delay).map_err(|error| Error::Smu {
        die: die.index(),
        error,
    })
}

/// The logical CPUs of all dies under `policy`.
pub fn thread_count<S: Soc, P: ConfigRegionAccess + Copy>(
    dies: &[Die<S, P>],
    policy: Policy,
) -> Result<usize, Error> {
    dies.iter()
        .map(|die| Threads::query(&die.smn(), policy).map(Threads::count))
        .sum()
}

/// Request `policy` for the next reset. Cold policy changes only take
/// effect after a reset: the platform owns that reset and must not release
/// any AP before it.
pub fn apply<S: Soc, P: ConfigRegionAccess + Copy>(
    die: &Die<S, P>,
    policy: Policy,
    delay: &mut impl DelayNs,
) -> Result<Effect, Error> {
    if policy == Policy::DEFAULT {
        return Ok(Effect::Unchanged);
    }
    let smn = die.smn();
    let mut config = smn.reg(THREAD_CONFIG).extract();
    let threads = Threads::decode(config)?;
    let requested = policy.downcore.request();
    if requested.is_some_and(|(cores, _)| cores > threads.cores_per_complex) {
        return Err(Error::CannotEnableCores);
    }
    let downcore = requested.filter(|&(cores, _)| cores < threads.cores_per_complex);
    let disable_smt = policy.disable_smt && threads.threads_per_core > 1;
    if let Some((_, mask)) = downcore {
        smn.reg(DOWNCORE).set(mask);
        test(die, delay)?;
    }
    if disable_smt {
        config.modify(ThreadConfig::DISABLE_SMT::SET);
        smn.reg(THREAD_CONFIG).set(config.get());
        test(die, delay)?;
    }
    Ok(if downcore.is_some() || disable_smt {
        Effect::ResetRequired
    } else {
        Effect::Unchanged
    })
}

/// S3 loses the policy registers; restoring them is a distinct operation,
/// not an atomic/cold-boot retry. The caller holds the retained policy.
pub fn restore<S: Soc, P: ConfigRegionAccess + Copy>(
    die: &Die<S, P>,
    policy: Policy,
    delay: &mut impl DelayNs,
) -> Result<(), Error> {
    let smn = die.smn();
    if let Some((_, mask)) = policy.downcore.request() {
        smn.reg(DOWNCORE).set(mask);
        test(die, delay)?;
    }
    if policy.disable_smt {
        smn.reg(THREAD_CONFIG)
            .modify(ThreadConfig::DISABLE_SMT::SET);
        test(die, delay)?;
    }
    Ok(())
}

/// Release `ap_count` APs from reset, die by die. The SMU releases a thread
/// when its ThreadEn bit ([`Threads::slots`]) gets set and the TEST message
/// is sent. Thread 0 of die 0 is the BSP. Software can only set ThreadEn
/// bits, not clear them. The APs start at the reset vector of the PSP's
/// copy of the BIOS image.
///
/// # Safety
/// `dies` are all dies in order and `policy` is the applied one. The code
/// at the AP reset vector and everything it needs to reach the platform's
/// AP entry is in place and stays alive until all APs have joined; errors
/// after a release require fail-stop/reset, not code reclamation.
pub unsafe fn launch<S: Soc, P: ConfigRegionAccess + Copy>(
    dies: &[Die<S, P>],
    policy: Policy,
    ap_count: usize,
    delay: &mut impl DelayNs,
) -> Result<(), Error> {
    let mut started = 0;
    for die in dies {
        // A die's topology is only read once its threads are needed.
        if started == ap_count {
            break;
        }
        let threads = Threads::query(&die.smn(), policy)?;
        let skip = usize::from(die.index() == 0);
        for slot in threads.slots().skip(skip).take(ap_count - started) {
            started += 1;
            let smn = die.smn();
            let thread_enable = smn.reg(THREAD_ENABLE);
            let enabled = thread_enable.get();
            if enabled == u32::MAX {
                return Err(Error::Unavailable { die: die.index() });
            }
            if enabled & (1 << slot) == 0 {
                fstart_log::debug!("SMU {}: ThreadEn bit {}", die.index(), slot);
                thread_enable.set(enabled | (1 << slot));
                test(die, delay)?;
            }
        }
    }
    if started != ap_count {
        fstart_log::error!("Can only start {} of {} APs", started, ap_count);
        return Err(Error::ApCount {
            started,
            requested: ap_count,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use crate::{fam17h::{topology::dies, tsc::testing::none}, smu::Mailbox};
    use crate::smn::{
        SmnReg,
        testing::{SmnModel, SmnTest},
    };
    use std::{collections::BTreeMap, vec::Vec};

    enum OneDie {}
    impl Soc for OneDie {
        const MAX_DIES: usize = 1;
        const SMU: Mailbox = Mailbox {
            message: SmnReg::new(0x03b1_0528),
            response: SmnReg::new(0x03b1_0564),
            arguments: SmnReg::new(0x03b1_0998),
        };
        const DXIO_SERVICE: u32 = 0x0a;
        fn ioms_fabric_id(_: u8) -> u8 {
            9
        }
    }

    #[derive(Default)]
    struct Registers {
        regs: BTreeMap<u32, u32>,
        writes: Vec<(u32, u32)>,
    }
    impl SmnModel for Registers {
        fn read(&mut self, address: u32) -> u32 {
            if address == OneDie::SMU.response.address() {
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
    fn die(test: &SmnTest<Registers>) -> Die<OneDie, &SmnTest<Registers>> {
        unsafe { dies(test) }.next().unwrap()
    }

    #[test]
    fn unavailable_thread_enable_is_not_an_already_released_cpu_set() {
        let test = SmnTest::new(Registers::default());
        test.model().regs.insert(THREAD_CONFIG.address(), 0x3); // 4 cores, SMT
        test.model().regs.insert(THREAD_ENABLE.address(), u32::MAX);
        assert_eq!(
            unsafe { launch(&[die(&test)], Policy::DEFAULT, 7, &mut none()) },
            Err(Error::Unavailable { die: 0 })
        );
        assert!(test.model().writes.is_empty());
    }

    #[test]
    fn masks_reset_requirement_and_complex_thread_stride() {
        let test = SmnTest::new(Registers::default());
        let die = die(&test);
        let reg = |r: u32| test.model().regs[&r];
        test.model().regs.insert(THREAD_CONFIG.address(), 0x13); // 4 cores, 2 complexes, SMT
        let policy = Policy {
            downcore: Downcore::Two,
            disable_smt: true,
        };
        assert_eq!(
            apply(&die, policy, &mut none()).unwrap(),
            Effect::ResetRequired
        );
        assert_eq!(reg(DOWNCORE.address()), 0xfc);
        assert_eq!(reg(THREAD_CONFIG.address()), 0x213);
        let threads = Threads::query(&die.smn(), policy).unwrap();
        assert_eq!(threads.slots().collect::<Vec<_>>(), [0, 1, 8, 9]);
        assert_eq!(thread_count(&[die], policy), Ok(4));
        unsafe { launch(&[die], policy, 3, &mut none()) }.unwrap();
        assert_eq!(reg(THREAD_ENABLE.address()), 0x302);
        test.model().writes.clear();
        assert_eq!(
            unsafe { launch(&[die], policy, 4, &mut none()) },
            Err(Error::ApCount {
                started: 3,
                requested: 4
            })
        );
        assert!(test.model().writes.is_empty());
        test.model().regs.insert(THREAD_CONFIG.address(), 0);
        assert_eq!(
            apply(&die, policy, &mut none()),
            Err(Error::CannotEnableCores)
        );
        assert!(test.model().writes.is_empty());
    }
}
