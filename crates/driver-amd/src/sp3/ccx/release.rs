//! First AP release through the SMU (CCX_SP3 §3.7).
//!
//! After reset only the BSP runs. Each further thread is started once by
//! setting its bit in SMU::PWR::THREAD_ENABLE and sending SMU message 1
//! (0xAA55AA55); later wakeups use INIT-SIPI. The thread then fetches the
//! reset vector (BIOS directory entry 0x62's copy), which the platform has
//! prepared, and reports to it.
//!
//! - Naples: one THREAD_ENABLE per die at SMN 0x5a818, on that die's SMN
//!   and SMU (AmdNbioSmuV9 `LaunchThread`; sp4r2 CCX_ZP §3.5 lead).
//! - Rome/Milan: one per CCD at 0x30081018 + physical CCD × 0x2000000, on
//!   the socket's SMN and SMU (SmuV11DxeGN `0x32fc`, decompiler-only).
//!
//! The bit is `complex * 8 + core * threads_per_core + thread` with
//! *logical* complex/core numbers (positions in the APOB 3/3 map; Oxide's
//! public `ccd.h` describes THREAD_ENABLE the same way); only the CCD uses
//! the physical ID. A bit that is already set fails (AGESA 0x80000007).

use fstart_pci::ConfigRegionAccess;
use heapless::Vec;
use tock_registers::LocalRegisterCopy;
use tock_registers::interfaces::{Readable, Writeable};
use tock_registers::register_bitfields;

use crate::sp3::apob::ccx::CoreMap;
use crate::sp3::smn::{Smn, SmnAddr};
use crate::sp3::smu::{SmuError, SmuMessages, send1};
use crate::sp3::soc::Generation;

/// SMU message 1 ("test"/apply, the same ID on V9 and V11) and its key.
pub const MSG_APPLY: u32 = 0x01;
pub const APPLY_KEY: u32 = 0xAA55_AA55;

/// Physical CCD stride of the CCD-local SMN registers.
pub const CCD_STRIDE: u32 = 0x0200_0000;

register_bitfields![u32,
    /// SMU::PWR::THREAD_CONFIGURATION.
    pub ThreadCfg [
        /// Cores per complex − 1.
        CORE_COUNT OFFSET(0) NUMBITS(4) [],
        /// Complexes − 1.
        COMPLEX_COUNT OFFSET(4) NUMBITS(4) [],
        /// 1 = one thread per core.
        SMT_MODE OFFSET(8) NUMBITS(1) [Smt = 0, OneThread = 1],
        /// Request SMT off from the next warm reset (CCX_SP3 §3.6).
        SMT_DISABLE OFFSET(9) NUMBITS(1) []
    ],
    /// SMU::PWR::SOFT_DOWNCORE.
    pub SoftDowncore [
        /// Disabled cores: \[3:0\] CCX 0, \[7:4\] CCX 1 (Milan: \[7:0\] one CCX);
        /// 0xff turns a Rome/Milan CCD off.
        DISCORE OFFSET(0) NUMBITS(8) []
    ]
];

/// The SMU::PWR registers of one die (Naples) or CCD (Rome/Milan).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SmuPwr {
    pub thread_enable: SmnAddr,
    pub thread_cfg: SmnAddr,
    pub soft_downcore: SmnAddr,
}

impl SmuPwr {
    /// Naples: per die, on the die's own SMN.
    pub const NAPLES: Self = Self {
        thread_enable: 0x0005_a818,
        thread_cfg: 0x0005_a81c,
        soft_downcore: 0x0005_a860,
    };

    /// Rome/Milan: physical CCD `ccd` of the socket.
    pub const fn ccd(ccd: u8) -> Self {
        let base = 0x3008_1000 + ccd as u32 * CCD_STRIDE;
        Self {
            thread_enable: base + 0x18,
            thread_cfg: base + 0x1c,
            soft_downcore: base + 0x20,
        }
    }
}

/// Topology of a Naples die from THREAD_CONFIGURATION.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DieThreads {
    pub complexes: u8,
    pub cores: u8,
    pub threads: u8,
}

impl DieThreads {
    /// Decode THREAD_CONFIGURATION; `None` for a value no ZP die has
    /// (more than 2 complexes of 4 cores, e.g. an all-ones read).
    pub fn from_thread_cfg(raw: u32) -> Option<Self> {
        let r = LocalRegisterCopy::<u32, ThreadCfg::Register>::new(raw);
        let t = Self {
            complexes: r.read(ThreadCfg::COMPLEX_COUNT) as u8 + 1,
            cores: r.read(ThreadCfg::CORE_COUNT) as u8 + 1,
            threads: if r.is_set(ThreadCfg::SMT_MODE) { 1 } else { 2 },
        };
        (t.complexes <= 2 && t.cores <= 4).then_some(t)
    }
}

/// One thread to start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Launch {
    pub socket: u8,
    /// Naples die; 0 on Rome/Milan.
    pub die: u8,
    /// THREAD_ENABLE of the die/CCD.
    pub thread_enable: SmnAddr,
    pub bit: u8,
}

/// Most threads per system: 2 sockets × 64 cores × 2.
pub const MAX_THREADS: usize = 256;

const fn bit(complex: usize, core: usize, threads: usize, thread: usize) -> u8 {
    (complex * 8 + core * threads + thread) as u8
}

/// Launch order for Naples: socket → die → complex → core → thread, skipping
/// the BSP (socket 0, die 0, first thread). `dies[s][d]` is the die's
/// THREAD_CONFIGURATION, `None` if the die is absent.
pub fn naples_order(dies: &[[Option<u32>; 4]]) -> Result<Vec<Launch, MAX_THREADS>, ReleaseError> {
    if dies.len() > Generation::MAX_SOCKETS {
        return Err(ReleaseError::BadTopology {
            socket: dies.len() as u8,
            die: 0,
        });
    }
    let topo: Vec<(usize, usize, DieThreads), 8> = dies
        .iter()
        .enumerate()
        .flat_map(|(s, die)| die.iter().enumerate().map(move |(d, cfg)| (s, d, *cfg)))
        .filter_map(|(s, d, cfg)| cfg.map(|c| (s, d, c)))
        .map(|(s, d, c)| {
            DieThreads::from_thread_cfg(c)
                .map(|t| (s, d, t))
                .ok_or(ReleaseError::BadTopology {
                    socket: s as u8,
                    die: d as u8,
                })
        })
        .collect::<Result<_, _>>()?;
    let per_die = |s: usize, d: usize, t: DieThreads| {
        let threads = t.threads as usize;
        (0..t.complexes as usize).flat_map(move |x| {
            (0..t.cores as usize).flat_map(move |c| {
                (0..threads).map(move |th| Launch {
                    socket: s as u8,
                    die: d as u8,
                    thread_enable: SmuPwr::NAPLES.thread_enable,
                    bit: bit(x, c, threads, th),
                })
            })
        })
    };
    // At most 2 sockets × 4 dies × 16 threads: fits.
    Ok(topo
        .into_iter()
        .flat_map(|(s, d, t)| per_die(s, d, t))
        .skip(1)
        .collect())
}

/// Launch order for Rome/Milan: socket → CCD → complex → core → thread from
/// the APOB maps (`maps[socket]`), skipping the BSP (first thread of the
/// first CCD of socket 0).
pub fn sp3_order(maps: &[CoreMap]) -> Result<Vec<Launch, MAX_THREADS>, ReleaseError> {
    maps.iter()
        .enumerate()
        .flat_map(|(s, map)| {
            // Uniform within a socket, as the SMU service assumes.
            let smt = map.cores().next().is_some_and(|(_, _, c)| c.threads[1]);
            let threads = if smt { 2 } else { 1 };
            map.ccds.iter().flat_map(move |ccd| {
                let te = SmuPwr::ccd(ccd.id).thread_enable;
                ccd.ccxs.iter().enumerate().flat_map(move |(x, ccx)| {
                    ccx.cores.iter().enumerate().flat_map(move |(c, core)| {
                        (0..threads)
                            .filter(move |&t| core.threads[t])
                            .map(move |t| Launch {
                                socket: s as u8,
                                die: 0,
                                thread_enable: te,
                                bit: bit(x, c, threads, t),
                            })
                    })
                })
            })
        })
        .skip(1)
        // A map larger than any SP3 system is an error, not a panic.
        .try_fold(Vec::new(), |mut v, l| {
            v.push(l).map_err(|l| ReleaseError::BadTopology {
                socket: l.socket,
                die: 0,
            })?;
            Ok(v)
        })
}

/// Why a launch failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseError {
    /// The thread's enable bit is already set.
    AlreadyRunning,
    /// A die reports an impossible topology, or there are too many sockets.
    BadTopology {
        socket: u8,
        die: u8,
    },
    Smu(SmuError),
}

impl From<SmuError> for ReleaseError {
    fn from(e: SmuError) -> Self {
        Self::Smu(e)
    }
}

/// Start one thread: set its THREAD_ENABLE bit and send the apply message.
/// `smn` and `smu` are those of the launch's die (Naples) or socket.
/// Waiting for the thread to report is the caller's job (bounded).
pub fn launch<P, M>(
    smn: &Smn<P>,
    smu: &M,
    l: &Launch,
    generation: Generation,
) -> Result<(), ReleaseError>
where
    P: ConfigRegionAccess + Clone,
    M: SmuMessages + ?Sized,
{
    // SAFETY: the BSP owns the SMN index/data pair during boot and
    // THREAD_ENABLE only starts threads.
    let reg = unsafe { smn.register::<()>(l.thread_enable) };
    // Rome/Milan use the low 16 bits only (SmuV11 `0x32fc`).
    let width: u32 = match generation {
        Generation::Naples => u32::MAX,
        _ => 0xffff,
    };
    let mask = (1u32 << l.bit) & width;
    let current = reg.get();
    if current & mask != 0 {
        return Err(ReleaseError::AlreadyRunning);
    }
    reg.set(current | mask);
    send1(smu, MSG_APPLY, APPLY_KEY)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sp3::apob::ccx::{Ccd, Ccx, Core};
    use crate::sp3::smn::model::Model;
    use core::cell::RefCell;

    #[derive(Default)]
    struct Smu(RefCell<std::vec::Vec<(u32, u32)>>);
    impl SmuMessages for Smu {
        fn send(&self, id: u32, args: &mut [u32; 6]) -> Result<(), SmuError> {
            self.0.borrow_mut().push((id, args[0]));
            Ok(())
        }
    }

    fn map(ccds: &[u8], cores: usize, smt: bool) -> CoreMap {
        CoreMap {
            ccds: ccds
                .iter()
                .map(|&id| Ccd {
                    id,
                    ccxs: [Ccx {
                        id: 0,
                        cores: (0..cores as u8)
                            .map(|c| Core {
                                id: c * 2,
                                threads: [true, smt],
                            })
                            .collect(),
                    }]
                    .into_iter()
                    .collect(),
                })
                .collect(),
        }
    }

    #[test]
    fn milan_order_uses_physical_ccd_and_logical_core() {
        let maps = [map(&[1, 6], 8, true), map(&[0], 2, true)];
        let order = sp3_order(&maps).unwrap();
        assert_eq!(order.len(), 16 + 16 + 4 - 1);
        assert_eq!(
            order[0],
            Launch {
                socket: 0,
                die: 0,
                thread_enable: 0x3208_1018,
                bit: 1
            }
        );
        // Core ids are 0, 2, 4..: the bit follows the position, not the id.
        assert_eq!(order[14].bit, 15);
        assert_eq!(order[15].thread_enable, 0x3008_1018 + 6 * CCD_STRIDE);
        assert_eq!(order.last().unwrap().socket, 1);
        let no_smt = sp3_order(&[map(&[0], 4, false)]).unwrap();
        assert_eq!(
            no_smt.iter().map(|l| l.bit).collect::<std::vec::Vec<_>>(),
            [1, 2, 3]
        );
    }

    #[test]
    fn naples_order_per_die() {
        // 2 CCX × 4 cores, SMT on die 0; die 1: 2 × 3 cores without SMT.
        let dies = [[Some(0x13), Some(0x112), None, None]];
        let order = naples_order(&dies).unwrap();
        let bad = [[Some(0x13), Some(0xffff_ffff), None, None]];
        assert_eq!(
            naples_order(&bad),
            Err(ReleaseError::BadTopology { socket: 0, die: 1 })
        );
        assert_eq!(order.len(), 16 + 6 - 1);
        assert_eq!(order[7].bit, 8, "CCX 1 starts at bit 8");
        assert_eq!(
            order[15],
            Launch {
                socket: 0,
                die: 1,
                thread_enable: 0x5a818,
                bit: 0
            }
        );
        assert_eq!(order[18].bit, 8);
    }

    #[test]
    fn launch_sets_the_bit_once() {
        let model = Model::default();
        let smn = Smn::new(model.clone(), 0);
        let smu = Smu::default();
        let l = Launch {
            socket: 0,
            die: 0,
            thread_enable: 0x3008_1018,
            bit: 3,
        };
        model.0.borrow_mut().smn.insert((0, 0x3008_1018), 0x1);
        launch(&smn, &smu, &l, Generation::Rome).unwrap();
        assert_eq!(model.0.borrow().smn[&(0, 0x3008_1018)], 0x9);
        assert_eq!(*smu.0.borrow(), [(1, 0xAA55_AA55)]);
        assert_eq!(
            launch(&smn, &smu, &l, Generation::Rome),
            Err(ReleaseError::AlreadyRunning)
        );
        assert_eq!(smu.0.borrow().len(), 1);
    }
}
