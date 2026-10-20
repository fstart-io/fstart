// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native Picasso CPU-driver adaptation. Construction is not AP-release approval.
use core::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use fstart_arch::x86::cpu::msr::{APIC_BASE, MsrAccess, NativeMsr};
use fstart_arch::x86::{
    cpuid,
    mp::{CpuDriver, CpuIdMatch, CpuIdentity, CpuVendor, MpError, SipiUnlock},
    mtrr::SipiMsrs,
};
use fstart_driver_amd::fam17h::nbio::{PSP_BASE_ADDR_HI, PSP_BASE_ADDR_LO};
use fstart_driver_amd::picasso::{
    cpu::{
        microcode::{self, Patch},
        thread,
    },
    registers::Smn,
    topology::Topology,
};
use tock_registers::interfaces::Readable;

const IDS: &[CpuIdMatch] = &[
    CpuIdMatch {
        vendor: CpuVendor::Amd,
        signature: 0x00810f10,
        mask: CpuIdMatch::ALL_STEPPINGS_MASK,
    },
    CpuIdMatch {
        vendor: CpuVendor::Amd,
        signature: 0x00810f80,
        mask: CpuIdMatch::ALL_STEPPINGS_MASK,
    },
    CpuIdMatch {
        vendor: CpuVendor::Amd,
        signature: 0x00820f00,
        mask: CpuIdMatch::ALL_STEPPINGS_MASK,
    },
];
const CAPACITY: usize = crate::layout::MP_CPU_CAPACITY as usize;
const LOCK_SPINS: usize = 100_000;
const _: () = assert!(CAPACITY > 0 && CAPACITY <= 16);

#[derive(Debug)]
pub enum Error {
    Identity,
    Topology,
    PspBar,
    Policy(thread::Error),
    Microcode(microcode::Error),
    Staging(crate::microcode::Error),
    Cache(crate::mp::Error),
    LockTimeout,
    Poisoned,
    CallbackOrder,
}

// OSRR56255: Fn8000001E is reserved with APIC_BAR.ApicEn clear.
// This architectural backend is xAPIC, never x2APIC MMIO.
fn apic_role(control: u64) -> Result<bool, Error> {
    if control & ((1 << 10) | (1 << 11)) != 1 << 11 {
        return Err(Error::Topology);
    }
    Ok(control & (1 << 8) != 0)
}

struct Facts {
    bsp: bool,
    signature: u32,
    apic_id: u32,
    total: u16,
    threads_per_core: u8,
}
impl Facts {
    fn read(msr: &impl MsrAccess) -> Result<Self, Error> {
        let identity = CpuIdentity::current();
        if !IDS.iter().any(|id| id.matches(identity)) {
            return Err(Error::Identity);
        }
        let (maximum, _, _, _) = cpuid(0x80000000);
        let (_, _, extensions, _) = cpuid(0x80000001);
        let (signature, ebx, _, features) = cpuid(1);
        // TopologyExtensions, MSRs, APIC, MTRRs, PAT and CLFLUSH are prerequisites.
        let required = (1 << 5) | (1 << 9) | (1 << 12) | (1 << 16) | (1 << 19);
        if maximum < 0x8000001e || extensions & (1 << 22) == 0 || features & required != required {
            return Err(Error::Identity);
        }
        let bsp = apic_role(msr.reg(APIC_BASE).get())?;
        let (apic_id, topology, _, _) = cpuid(0x8000001e);
        let threads = ((topology >> 8) & 0xff) + 1;
        let (_, _, count, _) = cpuid(0x80000008);
        if apic_id != ebx >> 24 || !(1..=2).contains(&threads) {
            return Err(Error::Topology);
        }
        Ok(Self {
            bsp,
            signature,
            apic_id,
            total: (count & 0xff) as u16 + 1,
            threads_per_core: threads as u8,
        })
    }
}

/// Complete caller-verified APIC set, with BSP first. SMU ThreadEn slot numbers
/// are deliberately NOT treated as APIC IDs, even when the counts agree.
struct CpuSet {
    ids: [u32; CAPACITY],
    count: usize,
}
impl CpuSet {
    fn new(ids: &[u32], bsp: u32, total: u16) -> Result<Self, Error> {
        if ids.is_empty()
            || ids.len() > CAPACITY
            || ids.len() != usize::from(total)
            || ids[0] != bsp
        {
            return Err(Error::Topology);
        }
        let mut result = Self {
            ids: [0; CAPACITY],
            count: ids.len(),
        };
        for (index, &id) in ids.iter().enumerate() {
            if id >= 255 || ids[..index].contains(&id) {
                return Err(Error::Topology);
            }
            result.ids[index] = id;
        }
        Ok(result)
    }
    fn index(&self, id: u32) -> Result<usize, Error> {
        self.ids[..self.count]
            .iter()
            .position(|&value| value == id)
            .ok_or(Error::Topology)
    }
    fn mask(&self) -> u16 {
        u16::MAX >> (16 - self.count)
    }
}

struct Preparation {
    facts: Facts,
    set: CpuSet,
    policy: thread::Policy,
}
impl Preparation {
    fn read(
        msr: &impl MsrAccess,
        smn: &Smn<impl fstart_pci::ConfigRegionAccess>,
        topology: Topology,
        apic_ids: &[u32],
        source: &[u8],
    ) -> Result<Self, Error> {
        let facts = Facts::read(msr)?;
        topology.thread_slots().map_err(|_| Error::Topology)?;
        if facts.total != u16::from(topology.logical_threads())
            || facts.threads_per_core != topology.threads_per_core
            || !facts.bsp
        {
            return Err(Error::Topology);
        }
        let set = CpuSet::new(apic_ids, facts.apic_id, facts.total)?;
        Patch::parse(source, facts.signature).map_err(Error::Microcode)?;
        // Only the BSP reads the already-installed/locked BAR; never reset a
        // PSP link to discover it, and never expose the shared port to APs.
        if smn.reg(PSP_BASE_ADDR_LO).get() != 0xfd100101 || smn.reg(PSP_BASE_ADDR_HI).get() != 0 {
            return Err(Error::PspBar);
        }
        let policy = thread::Policy::capture(msr, 0xfd100000).map_err(Error::Policy)?;
        Ok(Self { facts, set, policy })
    }
}

struct Serial {
    locked: AtomicBool,
    poisoned: AtomicBool,
}
impl Serial {
    const fn new() -> Self {
        Self {
            locked: AtomicBool::new(false),
            poisoned: AtomicBool::new(false),
        }
    }
    /// A bounded iteration budget, not an attested physical-time deadline.
    /// No Drop unlock, poisoned-owner reuse or automatic retry after failure.
    fn run<T>(&self, work: impl FnOnce() -> Result<T, Error>) -> Result<T, Error> {
        for _ in 0..LOCK_SPINS {
            if self.poisoned.load(Ordering::Acquire) {
                return Err(Error::Poisoned);
            }
            if self
                .locked
                .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
            {
                if self.poisoned.load(Ordering::Acquire) {
                    return Err(Error::Poisoned);
                }
                let result = work();
                if result.is_err() {
                    self.poisoned.store(true, Ordering::Release);
                }
                if self.poisoned.load(Ordering::Acquire) {
                    return result.and(Err(Error::Poisoned));
                }
                self.locked.store(false, Ordering::Release);
                return result;
            }
            core::hint::spin_loop();
        }
        self.poisoned.store(true, Ordering::Release);
        Err(Error::LockTimeout)
    }
}

/// Shared driver state is small and initialized outside the immutable image.
/// The MP borrow guard must retain this owner through callback completion and
/// terminal errors. Permanent patch bytes/code/mappings remain reserved to reset.
pub struct NativeCpu {
    set: CpuSet,
    signature: u32,
    threads_per_core: u8,
    policy: thread::Policy,
    patch: Patch<'static>,
    serial: Serial,
    patched: AtomicU16,
    initialized: AtomicU16,
    finished: AtomicBool,
}
impl NativeCpu {
    /// Prepare on the cold BSP after early native setup/topology policy, before
    /// releasing any AP. All checks precede CPU-policy writes.
    ///
    /// # Safety
    /// Supported CPU at ring 0, exclusive BSP MSR/SMN access, all intended APs
    /// held, valid asynchronous-event exclusion and physical cache semantics.
    /// `apic_ids` is the verified complete physical set with BSP first, NOT a
    /// dense count or ThreadEn-bit guess. It must agree with the SMU topology.
    /// Vendor patch bytes must occupy coherent identity-mapped immutable DRAM
    /// below4GiB, 16-byte aligned, with the WHOLE allocation reserved until reset.
    /// Keep this driver borrowed through MP completion/fail-stop, and all code,
    /// patch memory and AP resources mapped through reset. Later hooks must not
    /// change shared cache policy with other CPUs enabled. SMM/exception/reset,
    /// topology/reset-role applicability and stack budgets remain obligations.
    pub unsafe fn prepare(
        msr: &impl MsrAccess,
        smn: &Smn<impl fstart_pci::ConfigRegionAccess>,
        topology: Topology,
        apic_ids: &[u32],
        patch_bytes: &'static [u8],
    ) -> Result<Self, Error> {
        Patch::validate_address(patch_bytes.as_ptr() as u64).map_err(Error::Microcode)?;
        let preparation = Preparation::read(msr, smn, topology, apic_ids, patch_bytes)?;
        let patch =
            Patch::parse(patch_bytes, preparation.facts.signature).map_err(Error::Microcode)?;
        Ok(Self::from_preparation(preparation, patch))
    }
    /// Prepare from trusted temporary image bytes, copying only after complete
    /// CPU/topology/patch/BAR/routing preflight into the platform's permanent page.
    /// All results after its one-way claim retain that page until reset.
    ///
    /// # Safety
    /// The same cold BSP/AP/map/event/cache/reset/SMM contracts as `prepare`, plus
    /// `microcode::stage`'s entire-page physical and map-consumer contracts. The
    /// source is trusted/authenticated, readable and immutable through copying;
    /// it need not itself remain below4GiB or resident after this returns. It must
    /// be disjoint from the whole destination allocation. No loader executes here.
    pub unsafe fn prepare_staged(
        msr: &impl MsrAccess,
        smn: &Smn<impl fstart_pci::ConfigRegionAccess>,
        topology: Topology,
        apic_ids: &[u32],
        source: &[u8],
        memory: &crate::memory::Memory,
    ) -> Result<Self, Error> {
        let preparation = Preparation::read(msr, smn, topology, apic_ids, source)?;
        let resident =
            unsafe { crate::microcode::stage(source, preparation.facts.signature, memory) }
                .map_err(Error::Staging)?;
        let patch = Patch::parse(resident.bytes(), preparation.facts.signature)
            .map_err(Error::Microcode)?;
        Ok(Self::from_preparation(preparation, patch))
    }
    fn from_preparation(preparation: Preparation, patch: Patch<'static>) -> Self {
        Self {
            set: preparation.set,
            signature: preparation.facts.signature,
            threads_per_core: preparation.facts.threads_per_core,
            policy: preparation.policy,
            patch,
            serial: Serial::new(),
            patched: AtomicU16::new(0),
            initialized: AtomicU16::new(0),
            finished: AtomicBool::new(false),
        }
    }
    fn local(&self, msr: &impl MsrAccess) -> Result<(usize, bool), Error> {
        let facts = Facts::read(msr)?;
        if facts.signature != self.signature
            || facts.threads_per_core != self.threads_per_core
            || usize::from(facts.total) != self.set.count
        {
            return Err(Error::Identity);
        }
        let index = self.set.index(facts.apic_id)?;
        if facts.bsp != (index == 0) {
            return Err(Error::Topology);
        }
        Ok((
            index,
            facts
                .apic_id
                .is_multiple_of(u32::from(self.threads_per_core)),
        ))
    }
    fn stop(&self) -> ! {
        self.serial.poisoned.store(true, Ordering::Release);
        loop {
            unsafe {
                core::arch::asm!("cli", "hlt", options(nomem, nostack));
            }
        }
    }
    fn work_complete(&self) -> bool {
        !self.serial.poisoned.load(Ordering::Acquire)
            && !self.serial.locked.load(Ordering::Acquire)
            && self.patched.load(Ordering::Acquire) == self.set.mask()
            && self.initialized.load(Ordering::Acquire) == self.set.mask()
    }
    /// Use after successful architectural MP completion. Only its BSP post-MP
    /// hook, after the callback-return barrier, publishes this flag. Calling
    /// CPU-driver hooks outside that protocol violates the constructor contract.
    /// This does not establish SMM, graphics or general silicon completion.
    pub fn complete(&self) -> bool {
        self.finished.load(Ordering::Acquire) && self.work_complete()
    }
}
impl CpuDriver for NativeCpu {
    fn name(&self) -> &'static str {
        "native-picasso"
    }
    fn id_table(&self) -> &'static [CpuIdMatch] {
        IDS
    }
    fn sipi_msrs(&self) -> Result<SipiMsrs, MpError> {
        self.serial
            .run(|| {
                let msr = unsafe { NativeMsr::new() };
                if self.local(&msr)?.0 != 0 {
                    return Err(Error::Topology);
                }
                self.policy.validate(&msr).map_err(Error::Policy)?;
                crate::mp::sipi_msrs(&msr).map_err(Error::Cache)
            })
            .map_err(|_| MpError::CacheabilitySetupFailed)
    }
    fn sipi_unlock(&self) -> Option<SipiUnlock> {
        Some(crate::mp::SIPI_UNLOCK)
    }
    fn update_microcode(&self) {
        if self
            .serial
            .run(|| {
                let msr = unsafe { NativeMsr::new() };
                let (index, _) = self.local(&msr)?;
                let bit = 1u16 << index;
                if self.patched.load(Ordering::Relaxed) & bit != 0 {
                    return Err(Error::CallbackOrder);
                }
                // Constructor owns the permanent immutable physical patch contract.
                unsafe {
                    self.patch
                        .apply(&msr, self.patch.bytes().as_ptr() as u64, self.signature)
                }
                .map_err(Error::Microcode)?;
                self.patched.fetch_or(bit, Ordering::Release);
                Ok(())
            })
            .is_err()
        {
            self.stop();
        }
    }
    fn init_cpu(&self) {
        if self
            .serial
            .run(|| {
                let msr = unsafe { NativeMsr::new() };
                let (index, primary) = self.local(&msr)?;
                let bit = 1u16 << index;
                if self.patched.load(Ordering::Relaxed) & bit == 0
                    || self.initialized.load(Ordering::Relaxed) & bit != 0
                {
                    return Err(Error::CallbackOrder);
                }
                self.policy
                    .initialize(&msr, index == 0, primary)
                    .map_err(Error::Policy)?;
                crate::mp::hide_fixed_dram_routing(&msr);
                self.initialized.fetch_or(bit, Ordering::Release);
                Ok(())
            })
            .is_err()
        {
            self.stop();
        }
    }
    fn post_mp_init(&self) {
        let msr = unsafe { NativeMsr::new() };
        if !matches!(self.local(&msr), Ok((0, _)))
            || !self.work_complete()
            || self.finished.swap(true, Ordering::AcqRel)
        {
            self.stop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn topology_requires_enabled_xapic_before_extended_id_use() {
        assert!(apic_role(0).is_err());
        assert!(apic_role(1 << 8).is_err());
        assert!(apic_role((1 << 10) | (1 << 11)).is_err());
        assert!(!apic_role(1 << 11).unwrap());
        assert!(apic_role((1 << 8) | (1 << 11)).unwrap());
    }
    #[test]
    fn serial_success_unlocks_but_failure_and_timeout_are_terminal() {
        let gate = Serial::new();
        assert_eq!(gate.run(|| Ok::<_, Error>(7)).unwrap(), 7);
        assert!(!gate.locked.load(Ordering::Acquire));
        assert!(matches!(
            gate.run(|| Err::<(), _>(Error::CallbackOrder)),
            Err(Error::CallbackOrder)
        ));
        assert!(gate.locked.load(Ordering::Acquire));
        assert!(matches!(
            gate.run::<()>(|| panic!("poisoned owner must not execute")),
            Err(Error::Poisoned)
        ));
        let busy = Serial::new();
        busy.locked.store(true, Ordering::Release);
        assert!(matches!(
            busy.run::<()>(|| panic!("busy owner must not execute")),
            Err(Error::LockTimeout)
        ));
        assert!(busy.locked.load(Ordering::Acquire));
        assert!(matches!(
            busy.run::<()>(|| panic!("timeout must not retry")),
            Err(Error::Poisoned)
        ));
    }
    #[test]
    fn physical_cpu_set_is_not_inferred_from_count_or_smu_slots() {
        let set = CpuSet::new(&[0, 2, 8, 10], 0, 4).unwrap();
        assert_eq!(set.index(8).unwrap(), 2);
        assert!(set.index(1).is_err());
        assert_eq!(set.mask(), 15);
        for ids in [
            &[0, 0][..],
            &[1, 2][..],
            &[0, 255][..],
            &[0, 256][..],
            &[0][..],
        ] {
            assert!(CpuSet::new(ids, 0, 2).is_err());
        }
        let all: [u32; 16] = core::array::from_fn(|i| i as u32);
        assert_eq!(CpuSet::new(&all, 0, 16).unwrap().mask(), u16::MAX);
    }
}
