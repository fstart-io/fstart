// SPDX-License-Identifier: MIT OR Apache-2.0
//! Zeppelin SMU V9 mailbox and the messages
//! the native init sends. The same SMN addresses on every die.
//!
//! Call order: [`print_name`] early; [`init`] and [`cac_weights`] before
//! MP init;
//! [`crate::fam17h::topology::launch`] to release the APs; [`aps_ready`]
//! then [`ccx_done`] after every CPU ran [`super::ccx::cpu_init`].
use crate::fam17h::tsc::DelayNs;
use super::{Die, MAX_DIES};
use crate::fam17h::ccx::{CAC_WEIGHT_COUNT, CacWeights};
pub use crate::fam17h::smu::{ARGUMENT_COUNT, Error, Reply};
use crate::fam17h::smu::Mailbox;
use crate::smn::SmnReg;
use fstart_pci::ConfigRegionAccess;
use zerocopy::{
    Immutable, IntoBytes, KnownLayout, Unaligned,
    byteorder::{LittleEndian, U32},
};

pub const MAILBOX: Mailbox = Mailbox {
    message: SmnReg::new(0x03b1_0528),
    response: SmnReg::new(0x03b1_0564),
    arguments: SmnReg::new(0x03b1_0598),
};

pub use crate::fam17h::smu::TEST;
/// Argument: the expected interface version.
pub const GET_VERSION: u32 = 0x03;
/// Argument: feature mask.
pub const ENABLE_FEATURES: u32 = 0x09;
pub const DXIO_SERVICE: u32 = 0x0f;
/// Argument: index 0..20.
pub const GET_CAC_WEIGHT: u32 = 0x10;
pub const S3_ENTRY: u32 = 0x13;
pub const SET_POLICY_ADDRESS_HI: u32 = 0x1d;
pub const SET_POLICY_ADDRESS_LO: u32 = 0x1e;
pub const SET_AGM_LOG_ADDRESS_HI: u32 = 0x1f;
pub const SET_AGM_LOG_ADDRESS_LO: u32 = 0x20;
pub const TRANSFER_POLICY: u32 = 0x22;
/// Argument: 0.
pub const DC_BTC: u32 = 0x26;
/// Argument: dword index.
pub const GET_NAME_STRING: u32 = 0x28;
pub const SET_PERFMON_ADDRESS_HI: u32 = 0x2d;
pub const SET_PERFMON_ADDRESS_LO: u32 = 0x2e;
pub const MSG_38: u32 = 0x38;
pub const MSG_39: u32 = 0x39;
/// Argument: root bus of the die.
pub const MSG_3B: u32 = 0x3b;
/// Argument: root bus of the die.
pub const MSG_3D: u32 = 0x3d;
pub const CCX_DONE: u32 = 0x41;

/// SMU features enabled on SP4 and SP4r2 (vendor PCD 0xbb).
const FEATURES_SP4: u32 = 0xe73d;
const FEATURE_CORE_CSTATES: u32 = 1 << 8;
const INTERFACE_VERSION: u32 = 7;

/// A failed SMU interaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    /// A message to a die's SMU didn't complete successfully.
    Message { die: u8, message: u32, error: Error },
    /// A DMA buffer address is unaligned or missing.
    Buffer,
}

fn send<P: ConfigRegionAccess + Copy>(
    die: &Die<P>,
    message: u32,
    argument: u32,
    delay: &mut impl DelayNs,
) -> Result<[u32; ARGUMENT_COUNT], Failure> {
    die.smu(message, [argument, 0, 0, 0, 0, 0], delay)
        .map_err(|error| {
            fstart_log::error!(
                "SMU {} message {} failed",
                die.index(),
                fstart_log::Hex(u64::from(message))
            );
            Failure::Message {
                die: die.index(),
                message,
                error,
            }
        })
}

/// The 47-character CPU name string from die 0's SMU.
pub fn cpu_name<P: ConfigRegionAccess + Copy>(
    die: &Die<P>,
    delay: &mut impl DelayNs,
) -> Result<[u8; 48], Failure> {
    MAILBOX
        .cpu_name(&die.smn(), GET_NAME_STRING, delay)
        .map_err(|error| Failure::Message {
            die: die.index(),
            message: GET_NAME_STRING,
            error,
        })
}

/// Log the CPU name, silently skipped if the SMU doesn't answer.
pub fn print_name<P: ConfigRegionAccess + Copy>(die: &Die<P>, delay: &mut impl DelayNs) {
    if let Ok(name) = cpu_name(die, delay) {
        let length = name.iter().position(|&c| c == 0).unwrap_or(name.len());
        let name = core::str::from_utf8(&name[..length]).unwrap_or("?");
        fstart_log::info!("CPU: {}", name);
    }
}

/// BIOS policy table of the SMU V9 firmware. Zero selects the fused value.
/// The vendor SMU driver sets the determinism fields itself; everything
/// else comes from PCDs that default to 0.
#[repr(C)]
#[derive(Debug, Clone, Copy, IntoBytes, Immutable, KnownLayout, Unaligned)]
pub struct PolicyTable {
    /// MaxSusPwr.
    tdp: U32<LittleEndian>,
    tdc: U32<LittleEndian>,
    edc: U32<LittleEndian>,
    determinism_enable: u8,
    reserved_0d: [u8; 3],
    determinism_percent: U32<LittleEndian>,
    reserved_14: [u8; 0x7c - 0x14],
}
const _: () = assert!(size_of::<PolicyTable>() == 0x7c);
impl PolicyTable {
    /// What the vendor firmware hands the SMU on SP4 and SP4r2.
    pub const SP4: Self = Self {
        tdp: U32::ZERO,
        tdc: U32::ZERO,
        edc: U32::ZERO,
        determinism_enable: 1,
        reserved_0d: [0; 3],
        determinism_percent: U32::new(100),
        reserved_14: [0; 0x7c - 0x14],
    };
}

/// Physical addresses of the DRAM the SMU firmware reads its policy from
/// and writes its AGM log and performance monitor data to.
#[derive(Debug, Clone, Copy)]
pub struct DmaBuffers {
    /// A [`PolicyTable`], 64-byte aligned.
    pub policy: u64,
    /// One zeroed 4-KiB page per die.
    pub agm_log: [u64; MAX_DIES],
    /// One zeroed 4-KiB page per die.
    pub perfmon: [u64; MAX_DIES],
}
impl DmaBuffers {
    fn valid(&self, dies: usize) -> bool {
        let page = |address: u64| address != 0 && address & 0xfff == 0;
        self.policy != 0
            && self.policy & 63 == 0
            && dies <= MAX_DIES
            && self.agm_log[..dies].iter().all(|&a| page(a))
            && self.perfmon[..dies].iter().all(|&a| page(a))
    }
}

fn set_address<P: ConfigRegionAccess + Copy>(
    die: &Die<P>,
    (low, high): (u32, u32),
    address: u64,
    delay: &mut impl DelayNs,
) -> Result<(), Failure> {
    send(die, low, address as u32, delay)?;
    send(die, high, (address >> 32) as u32, delay).map(|_| ())
}

/// `zeppelin_smu_init`, the SMU DXE driver of the vendor firmware
/// (AmdNbioSmuV9Dxe entry), per die. The SMU gets its AGM log and perfmon
/// buffers before it enables features.
///
/// # Safety
/// `dies` are all dies in order. `buffers.policy` holds [`PolicyTable::SP4`]
/// and the AGM log and perfmon pages are zeroed. All of them are DRAM the
/// platform reserves from the OS until reset: the SMU keeps using them.
pub unsafe fn init<P: ConfigRegionAccess + Copy>(
    dies: &[Die<P>],
    buffers: &DmaBuffers,
    delay: &mut impl DelayNs,
) -> Result<(), Failure> {
    if !buffers.valid(dies.len()) {
        return Err(Failure::Buffer);
    }
    dies.iter().try_for_each(|die| {
        let index = usize::from(die.index());
        let bus = u32::from(die.bus());
        // The vendor code asserts (AGESA event 0xa2411310) and goes on.
        let version = send(die, GET_VERSION, INTERFACE_VERSION, delay)?[0];
        if version != INTERFACE_VERSION {
            fstart_log::error!(
                "SMU {}: interface version {}, expected {}",
                die.index(),
                version,
                INTERFACE_VERSION
            );
        }
        let policy = (SET_POLICY_ADDRESS_LO, SET_POLICY_ADDRESS_HI);
        let agm_log = (SET_AGM_LOG_ADDRESS_LO, SET_AGM_LOG_ADDRESS_HI);
        let perfmon = (SET_PERFMON_ADDRESS_LO, SET_PERFMON_ADDRESS_HI);
        set_address(die, policy, buffers.policy, delay)?;
        send(die, TRANSFER_POLICY, 0, delay)?;
        set_address(die, agm_log, buffers.agm_log[index], delay)?;
        set_address(die, perfmon, buffers.perfmon[index], delay)?;
        send(die, MSG_3B, bus, delay)?;
        send(die, MSG_3D, bus, delay).map(|_| ())
    })
}

/// The CAC weights the CCX code loads into the DPM weight MSRs, from die 0.
/// Before MP init: the APs can't talk to the SMU in parallel.
pub fn cac_weights<P: ConfigRegionAccess + Copy>(
    die: &Die<P>,
    delay: &mut impl DelayNs,
) -> Result<CacWeights, Failure> {
    let mut weights = [0; CAC_WEIGHT_COUNT];
    for (index, weight) in (0..).zip(&mut weights) {
        let reply = send(die, GET_CAC_WEIGHT, index, delay)?;
        *weight = (u64::from(reply[0]) << 32) | u64::from(reply[1]);
    }
    Ok(weights)
}

/// The vendor SMU driver's MP-services callback, after the APs ran their
/// CPU init. AC BTC, which runs a workload on every AP with messages
/// 0x23/0x24/0x25, is off (vendor PCD 0x1bf = 0). DC BTC is on for SP4 and
/// SP4r2 (the SMU driver sets PCD 0x1be): message 0x26 with argument 0 to
/// die 0. Then every die enables the CORE_CSTATES feature.
pub fn aps_ready<P: ConfigRegionAccess + Copy>(
    dies: &[Die<P>],
    delay: &mut impl DelayNs,
) -> Result<(), Failure> {
    if let Some(first) = dies.first() {
        send(first, DC_BTC, 0, delay)?;
    }
    dies.iter().try_for_each(|die| {
        send(
            die,
            ENABLE_FEATURES,
            FEATURES_SP4 & FEATURE_CORE_CSTATES,
            delay,
        )
        .map(|_| ())
    })
}

/// CallbackAfterCcxDxeDone of the vendor SMU driver, per die. After
/// [`aps_ready`].
pub fn ccx_done<P: ConfigRegionAccess + Copy>(
    dies: &[Die<P>],
    delay: &mut impl DelayNs,
) -> Result<(), Failure> {
    dies.iter().try_for_each(|die| {
        send(die, CCX_DONE, 0, delay)?;
        send(die, ENABLE_FEATURES, FEATURES_SP4, delay)?;
        send(die, MSG_38, 0, delay)?;
        send(die, MSG_39, 0, delay).map(|_| ())
    })
}

#[cfg(test)]
mod tests {
    extern crate std;
    use crate::fam17h::tsc::testing::Delay;
    use super::{
        super::test_pci::{Message, Pci},
        *,
    };
    use std::vec::Vec;

    fn sent(pci: &Pci) -> Vec<(u8, u32, u32)> {
        pci.messages()
            .iter()
            .map(|m| (m.die, m.message, m.arguments[0]))
            .collect()
    }

    #[test]
    fn launches_aps_across_dies_with_the_complex_stride() {
        use crate::fam17h::{
            smu::{THREAD_CONFIG, THREAD_ENABLE},
            topology::{self, Policy},
        };
        let pci = Pci::new(&[0, 0x40], |_| [0; 6]);
        pci.set_smn(0, THREAD_CONFIG, 0x013); // 4 cores, 2 complexes, SMT
        pci.set_smn(1, THREAD_CONFIG, 0x103); // 4 cores, 1 complex, no SMT
        pci.set_smn(0, THREAD_ENABLE, 0b11); // BSP and thread 1 already run
        let dies = pci.dies();
        assert_eq!(topology::thread_count(&dies, Policy::DEFAULT), Ok(20));
        unsafe { topology::launch(&dies, Policy::DEFAULT, 17, &mut Delay(|_| {})) }.unwrap();
        assert_eq!(pci.smn(0, THREAD_ENABLE), 0xffff);
        assert_eq!(pci.smn(1, THREAD_ENABLE), 0b11);
        let tests: Vec<_> = sent(&pci).iter().map(|&(die, ..)| die).collect();
        assert_eq!(tests.iter().filter(|&&die| die == 0).count(), 14);
        assert_eq!(tests.iter().filter(|&&die| die == 1).count(), 2);
        assert!(
            sent(&pci)
                .iter()
                .all(|&(_, m, a)| m == TEST && a == 0xaa55_aa55)
        );

        assert_eq!(
            unsafe { topology::launch(&dies, Policy::DEFAULT, 20, &mut Delay(|_| {})) },
            Err(topology::Error::ApCount {
                started: 19,
                requested: 20
            })
        );
        assert_eq!(pci.smn(1, THREAD_ENABLE), 0xf);
    }

    #[test]
    fn init_hands_each_die_its_buffers_and_root_bus() {
        let pci = Pci::new(&[0, 0x40], |m: &Message| {
            [if m.message == GET_VERSION { 7 } else { 0 }, 0, 0, 0, 0, 0]
        });
        let dies = pci.dies();
        let mut buffers = DmaBuffers {
            policy: 0x1_2345_6740,
            agm_log: [0x7000_0000, 0x7000_1000],
            perfmon: [0x7000_2000, 0],
        };
        assert_eq!(
            unsafe { init(&dies, &buffers, &mut Delay(|_| {})) },
            Err(Failure::Buffer)
        );
        assert!(pci.messages().is_empty());
        buffers.perfmon[1] = 0x7000_3000;
        unsafe { init(&dies, &buffers, &mut Delay(|_| {})) }.unwrap();
        let die1: Vec<_> = sent(&pci)
            .into_iter()
            .filter(|&(die, ..)| die == 1)
            .map(|(_, m, a)| (m, a))
            .collect();
        assert_eq!(
            die1,
            [
                (GET_VERSION, 7),
                (SET_POLICY_ADDRESS_LO, 0x2345_6740),
                (SET_POLICY_ADDRESS_HI, 1),
                (TRANSFER_POLICY, 0),
                (SET_AGM_LOG_ADDRESS_LO, 0x7000_1000),
                (SET_AGM_LOG_ADDRESS_HI, 0),
                (SET_PERFMON_ADDRESS_LO, 0x7000_3000),
                (SET_PERFMON_ADDRESS_HI, 0),
                (MSG_3B, 0x40),
                (MSG_3D, 0x40),
            ]
        );
        assert_eq!(PolicyTable::SP4.as_bytes()[0x0c], 1);
        assert_eq!(PolicyTable::SP4.as_bytes()[0x10], 100);
    }

    #[test]
    fn cac_weights_and_post_mp_messages() {
        let pci = Pci::new(&[0, 0x40], |m: &Message| {
            [0x100 + m.arguments[0], m.arguments[0], 0, 0, 0, 0]
        });
        let dies = pci.dies();
        let weights = cac_weights(&dies[0], &mut Delay(|_| {})).unwrap();
        assert_eq!(weights[20], 0x114_0000_0014);
        assert!(
            sent(&pci)
                .iter()
                .all(|&(die, m, _)| die == 0 && m == GET_CAC_WEIGHT)
        );

        let pci = Pci::new(&[0, 0x40], |_| [0; 6]);
        let dies = pci.dies();
        aps_ready(&dies, &mut Delay(|_| {})).unwrap();
        ccx_done(&dies, &mut Delay(|_| {})).unwrap();
        assert_eq!(
            sent(&pci),
            [
                (0, DC_BTC, 0),
                (0, ENABLE_FEATURES, 0x100),
                (1, ENABLE_FEATURES, 0x100),
                (0, CCX_DONE, 0),
                (0, ENABLE_FEATURES, 0xe73d),
                (0, MSG_38, 0),
                (0, MSG_39, 0),
                (1, CCX_DONE, 0),
                (1, ENABLE_FEATURES, 0xe73d),
                (1, MSG_38, 0),
                (1, MSG_39, 0),
            ]
        );
    }
}
