// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native CPU cold flow: SMU AP release, microcode and per-CPU init through
//! the architectural MP engine, then the later CCX-done notification.
use crate::{
    cpu::{Fam17hCpu, PatchPage},
    layout,
    memory::Memory,
};
use core::mem::MaybeUninit;
use fstart_arch::x86::{
    cpu::msr::{APIC_BASE, ApicBase, MsrAccess, NativeMsr},
    mp::{self as arch_mp, CpuDriver, MpConfig, MpError, MpHandle},
};
use fstart_driver_amd::{
    fam17h::{
        topology::{self, Policy},
        tsc::DelayNs,
    },
    picasso::{
        Die, Picasso,
        cpu::thread,
        nbio::{self, PSP_MMIO_BASE},
        nbio_late, power,
    },
    smn::Smn,
};
use tock_registers::interfaces::Readable;

pub struct Inputs<'a> {
    /// Already applied thread policy: no policy reset/reconfiguration here.
    pub policy: Policy,
    /// Complete physical APIC IDs, BSP first. Not ThreadEn slots.
    pub apic_ids: &'a [u32],
    /// The whole boot flash mapped below 4 GiB, with the microcode patches
    /// in its BIOS directory.
    pub flash: &'a [u8],
    pub memory: &'a Memory,
}

/// The microcode patch the CPUs load, in the permanent page the linker
/// script reserves.
#[repr(C, align(4096))]
struct MicrocodePage(MaybeUninit<PatchPage>);
#[used]
#[unsafe(link_section = ".picasso_microcode")]
static mut MICROCODE: MicrocodePage = MicrocodePage(MaybeUninit::uninit());
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LateResponse {
    pub command: u32,
    pub status: u32,
}
/// SMU replies to the late notifications, kept for diagnostics.
#[derive(Debug)]
pub struct LateResponses {
    entries: [LateResponse; 8],
    count: usize,
}
impl LateResponses {
    fn new() -> Self {
        Self {
            entries: [LateResponse {
                command: 0,
                status: 0,
            }; 8],
            count: 0,
        }
    }
    fn push(&mut self, command: u32, status: u32) -> bool {
        let Some(entry) = self.entries.get_mut(self.count) else {
            return false;
        };
        *entry = LateResponse { command, status };
        self.count += 1;
        true
    }
    pub fn entries(&self) -> &[LateResponse] {
        &self.entries[..self.count]
    }
}
#[derive(Debug)]
pub enum Error {
    Topology(topology::Error),
    CpuCount {
        expected: usize,
        actual: usize,
    },
    MicrocodeNotReserved,
    PspBar,
    Policy(thread::Error),
    Mp(MpError),
    ApRelease(topology::Error),
    IncompleteCpu,
    Smu {
        cause: power::Error,
        responses: LateResponses,
    },
    DiagnosticsOverflow,
}
/// Architectural CPU initialization completed. CCX-done is a later phase.
#[must_use]
pub struct CpusInitialized {
    mp: MpHandle,
}
impl CpusInitialized {
    pub fn mp(&self) -> &MpHandle {
        &self.mp
    }
    /// Send CCX-done and the late SMU power notifications.
    /// Run after native silicon/pre-enumeration/SMM setup.
    pub fn finish_ccx(
        self,
        graphics: bool,
        smn: &Smn<impl fstart_pci::ConfigRegionAccess>,
        delay: &mut impl DelayNs,
    ) -> Result<CcxNotified, Error> {
        let responses = notify(true, graphics, smn, delay)?;
        Ok(CcxNotified {
            cpus: self,
            responses,
        })
    }
}
#[must_use]
pub struct CcxNotified {
    cpus: CpusInitialized,
    responses: LateResponses,
}
impl CcxNotified {
    pub fn mp(&self) -> &MpHandle {
        self.cpus.mp()
    }
    pub fn late_responses(&self) -> &[LateResponse] {
        self.responses.entries()
    }
}
fn count(expected: usize, ids: &[u32]) -> Result<u16, Error> {
    if ids.len() != expected {
        return Err(Error::CpuCount {
            expected,
            actual: ids.len(),
        });
    }
    Ok(expected as u16)
}
fn notify(
    complete: bool,
    graphics: bool,
    smn: &Smn<impl fstart_pci::ConfigRegionAccess>,
    delay: &mut impl DelayNs,
) -> Result<LateResponses, Error> {
    if !complete {
        return Err(Error::IncompleteCpu);
    }
    let mut responses = LateResponses::new();
    let mut overflow = false;
    let result = nbio_late::ccx_done(smn, graphics, delay, &mut |command, status| {
        overflow |= !responses.push(command, status);
    });
    if overflow {
        return Err(Error::DiagnosticsOverflow);
    }
    match result {
        Ok(()) => Ok(responses),
        Err(cause) => Err(Error::Smu { cause, responses }),
    }
}
/// Release the APs through the SMU and run microcode and per-CPU init.
/// # Safety
/// Cold boot after native early init and topology policy, all APs held, the
/// early firmware region reserved in `input.memory`. Exclusive MSR/SMN access.
pub unsafe fn initialize_cpus(
    input: Inputs<'_>,
    msr: &impl MsrAccess,
    die: &Die<impl fstart_pci::ConfigRegionAccess + Copy>,
    delay: &mut impl DelayNs,
) -> Result<CpusInitialized, Error> {
    let dies = core::slice::from_ref(die);
    let threads = topology::thread_count(dies, input.policy).map_err(Error::Topology)?;
    let total = count(threads, input.apic_ids)?;
    let smn = &die.smn();
    if !input.memory.microcode_reserved() {
        return Err(Error::MicrocodeNotReserved);
    }
    // Only the BSP reads the already-installed/locked BAR; never reset a
    // PSP link to discover it, and never expose the shared port to APs.
    if !nbio::psp_bar_installed(smn) {
        return Err(Error::PspBar);
    }
    let policy = thread::Policy::capture(msr, PSP_MMIO_BASE.into()).map_err(Error::Policy)?;
    // SAFETY: BSP before AP release; the linker reserves the page and the
    // memory map keeps it.
    let cpu = unsafe {
        Fam17hCpu::<Picasso, _>::new(
            "native-picasso",
            input.flash,
            &mut (*core::ptr::addr_of_mut!(MICROCODE)).0,
            move |msr: &NativeMsr, thread| {
                let bsp = msr.reg(APIC_BASE).is_set(ApicBase::BSP);
                policy.initialize(msr, bsp, thread)
            },
        )
    };
    let drivers: &[&dyn CpuDriver] = &[&cpu];
    let config = MpConfig {
        cpu_drivers: drivers,
        num_cpus: total,
        max_cpus: layout::MP_CPU_CAPACITY,
    };
    let mut release_error = None;
    let mut launch = |aps: u16| {
        // SAFETY: the SIPI trampoline is installed; released APs start at
        // the reset vector, where `picasso.S` sends them to it.
        let aps = usize::from(aps);
        unsafe { topology::launch(dies, input.policy, aps, delay) }.map_err(|error| {
            release_error = Some(error);
            0
        })
    };
    let mp = match arch_mp::mp_init_with_launcher(&config, &mut launch) {
        Ok(mp) => mp,
        Err(MpError::PlatformLaunchFailed(_)) => {
            return Err(Error::ApRelease(release_error.unwrap_or(
                topology::Error::ApCount {
                    started: 0,
                    requested: usize::from(total) - 1,
                },
            )));
        }
        Err(error) => return Err(Error::Mp(error)),
    };
    if mp.num_cpus() != total {
        return Err(Error::CpuCount {
            expected: usize::from(total),
            actual: usize::from(mp.num_cpus()),
        });
    }
    if cpu.errors() != 0 {
        return Err(Error::IncompleteCpu);
    }
    Ok(CpusInitialized { mp })
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use fstart_driver_amd::fam17h::tsc::testing::Delay;
    use fstart_driver_amd::{
        picasso::smu,
        smn::testing::{SmnModel, SmnTest},
    };
    use std::{collections::BTreeMap, vec::Vec};
    struct Deny;
    impl SmnModel for Deny {
        fn read(&mut self, _: u32) -> u32 {
            panic!("incomplete CPUs must not read SMN");
        }
        fn write(&mut self, _: u32, _: u32) {
            panic!("incomplete CPUs must not write SMN");
        }
    }
    #[test]
    fn cached_count_and_completion_gate_including_singleton() {
        assert!(matches!(
            count(8, &[0]),
            Err(Error::CpuCount {
                expected: 8,
                actual: 1
            })
        ));
        assert_eq!(count(8, &[0, 1, 4, 5, 8, 9, 12, 13]).unwrap(), 8);
        assert_eq!(count(1, &[42]).unwrap(), 1);
        assert!(matches!(
            notify(false, false, &SmnTest::new(Deny).smn(), &mut Delay(|_| panic!(
                "no delay before CPU completion"
            )),),
            Err(Error::IncompleteCpu)
        ));
    }
    struct Mailbox {
        registers: BTreeMap<u32, u32>,
        frames: Vec<(u32, [u32; 6])>,
        unavailable_after: usize,
    }
    impl SmnModel for Mailbox {
        fn read(&mut self, address: u32) -> u32 {
            *self.registers.get(&address).unwrap_or(&0)
        }
        fn write(&mut self, address: u32, value: u32) {
            self.registers.insert(address, value);
            if address == smu::MESSAGE.address() {
                let arguments =
                    core::array::from_fn(|i| self.read(smu::ARGUMENTS.address() + i as u32 * 4));
                self.frames.push((value, arguments));
                // Immediate synchronous CPU-only fake completion, not physical PSP/SMU evidence.
                self.registers.insert(
                    smu::RESPONSE.address(),
                    if self.frames.len() > self.unavailable_after {
                        u32::MAX
                    } else {
                        2
                    },
                );
            }
        }
    }
    fn mailbox(unavailable_after: usize) -> Mailbox {
        Mailbox {
            registers: BTreeMap::from([
                (smu::RESPONSE.address(), 1),
                (0x0005d5bc, 0x62e >> 5),
                (0x0005d5b8, (0x62e & 31) << 27),
            ]),
            frames: Vec::new(),
            unavailable_after,
        }
    }
    #[test]
    fn source_order_retains_all_nonstandard_responses_and_failure_prefix() {
        let transport = SmnTest::new(mailbox(usize::MAX));
        let records = notify(true, false, &transport.smn(), &mut Delay(|_| {
            panic!("immediate fake needs no delay")
        }))
        .unwrap();
        let commands: Vec<_> = transport.model().frames.iter().map(|frame| frame.0).collect();
        assert_eq!(commands, [0x35, 5, 0x0e, 0x53, 0x3a, 0x42, 0x32, 5]);
        assert_eq!(records.entries().len(), 8);
        assert!(
            records
                .entries()
                .iter()
                .zip(commands)
                .all(|(entry, command)| entry.command == command && entry.status == 2)
        );
        let transport = SmnTest::new(mailbox(1));
        let error = notify(true, false, &transport.smn(), &mut Delay(|_| {
            panic!("unavailable fake must not delay")
        }))
        .unwrap_err();
        assert!(
            matches!(error,Error::Smu { cause: power::Error::Smu(smu::Error::Unavailable),responses } if responses.entries()==[LateResponse { command: 0x35, status: 2 }])
        );
        assert_eq!(transport.model().frames.len(), 2);
        let mut full = LateResponses::new();
        for i in 0..8 {
            assert!(full.push(i, 2));
        }
        assert!(!full.push(99, 2));
        assert_eq!(full.entries().len(), 8);
    }
}
