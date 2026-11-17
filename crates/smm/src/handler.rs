#[cfg(any(feature = "stage-bin", test))]
use core::arch::asm;
#[cfg(any(feature = "stage-bin", test))]
use core::mem::{align_of, size_of};
use core::sync::atomic::Ordering;

#[cfg(any(feature = "stage-bin", test))]
use crate::runtime::{HANDLER_CONFIG_ALIGNMENT, HANDLER_CONFIG_CAPACITY};
pub use crate::runtime::{SmmEntryParams, SmmRuntime};

/// Debug console port used by [`debug_trace`]: QEMU's `-debugcon`. Real
/// chipsets decode other registers here (Bay Trail's PM1_EN at ACPI base
/// 0x400), so the SMM stage only calls it with the `debugcon` feature.
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
pub const DEBUGCON: u16 = 0x0402;

/// Per-entry view handed to the concrete SMM handler.
pub struct SmmContext<'a> {
    params: &'a mut SmmEntryParams,
    /// The generation of this CPU's SMI, taken as it entered
    /// ([`Self::first_owner`]).
    epoch: u32,
}

impl<'a> SmmContext<'a> {
    /// Build a context from the raw entry-parameter pointer.
    ///
    /// # Safety
    ///
    /// `params` must be null or the current CPU's loader-filled entry block.
    #[inline(always)]
    pub unsafe fn from_raw(params: *mut SmmEntryParams) -> Option<Self> {
        if params.is_null() {
            return None;
        }
        let params = unsafe { &mut *params };
        let epoch = ingress(params.runtime);
        Some(Self { params, epoch })
    }

    /// Whether this CPU owns the rendezvous as the first owner of the SMI
    /// it entered with: no other CPU took the rendezvous between its entry
    /// and its claim. Its SMI's generation is taken at entry: the one a
    /// CPU owns then, or else the next. A late CPU of an SMI whose owner
    /// had already claimed it (or released it) owns a later generation: a
    /// source that came meanwhile is the FCH's to dispatch, but the SMI's
    /// once-per-SMI work, done by the first owner, is not its. A CPU that
    /// enters only after its SMI's owner released and every other CPU
    /// left is the next SMI's.
    #[inline(always)]
    pub fn first_owner(&self) -> bool {
        if self.params.runtime == 0 {
            return false;
        }
        // SAFETY: `from_raw`'s contract: the installer's runtime block,
        // in locked SMRAM for the whole SMI.
        let runtime = unsafe { &*(self.params.runtime as *const SmmRuntime) };
        let rendezvous = runtime.rendezvous.load(Ordering::Acquire);
        owner_of(rendezvous) == self.cpu().wrapping_add(1)
            && generation_of(rendezvous) == self.epoch
    }

    /// Logical CPU index of the current entry.
    #[inline(always)]
    pub const fn cpu(&self) -> u32 {
        self.params.cpu
    }

    /// For the rendezvous owner: the CPUs held for it, itself and the ones
    /// parked for its generation (each acknowledged it once its local work,
    /// `enter_cpu`, was done), and the CPUs whose SMBASE points at this
    /// image. A CPU of a previous owner's on its way out to its RSM is not
    /// parked for this one, and not counted. With all of them held, no CPU
    /// runs the OS and none has local work left (EDK2's PiSmmCpuDxeSmm
    /// waits for that as `SmmWaitForApArrival`). `None` for a CPU that is
    /// not the owner.
    #[inline(always)]
    pub fn cpus_held(&self) -> Option<(u32, u32)> {
        if self.params.runtime == 0 {
            return None;
        }
        // SAFETY: `from_raw`'s contract: the installer's runtime block,
        // in locked SMRAM for the whole SMI.
        let runtime = unsafe { &*(self.params.runtime as *const SmmRuntime) };
        let rendezvous = runtime.rendezvous.load(Ordering::Acquire);
        if owner_of(rendezvous) != self.cpu().wrapping_add(1) {
            return None;
        }
        let held = parked_for(&runtime.parked, generation_of(rendezvous));
        Some((held + 1, runtime.num_cpus.into()))
    }

    #[cfg(any(feature = "stage-bin", test))]
    #[inline(always)]
    pub(crate) fn runtime_ptr(&self) -> Option<*mut SmmRuntime> {
        (self.params.runtime != 0).then_some(self.params.runtime as *mut SmmRuntime)
    }

    /// Borrow the immutable installer configuration directly from SMRAM.
    /// Avoid copying larger configurations through compiler-generated memcpy.
    ///
    /// Returns `None` unless the runtime records exactly `size_of::<T>()`
    /// bytes at an aligned offset inside SMRAM.
    ///
    /// # Safety
    ///
    /// `T` must be the type the installer wrote, and `params.runtime` must
    /// point to the live runtime block inside locked SMRAM. The configuration
    /// must remain immutable and disjoint from entry parameters for `'a`.
    #[cfg(any(feature = "stage-bin", test))]
    #[inline(always)]
    pub(crate) unsafe fn handler_config<T: Copy>(&self) -> Option<&'a T> {
        let runtime = self.runtime_ptr()?;
        // SAFETY: the runtime pointer was written by the installer and the
        // block lives in locked SMRAM for the whole SMI.
        let (offset, size, smram_base, smram_size) = unsafe {
            (
                (*runtime).handler_config_offset as usize,
                (*runtime).handler_config_size as usize,
                (*runtime).smram_base as usize,
                (*runtime).smram_size as usize,
            )
        };
        let start = (runtime as usize).checked_add(offset)?;
        let end = start.checked_add(size)?;
        if size != size_of::<T>()
            || size > HANDLER_CONFIG_CAPACITY
            || align_of::<T>() > HANDLER_CONFIG_ALIGNMENT
            || !offset.is_multiple_of(HANDLER_CONFIG_ALIGNMENT)
            || offset < size_of::<SmmRuntime>()
            || start < smram_base
            || end > smram_base.checked_add(smram_size)?
        {
            return None;
        }
        // SAFETY: the range was validated above and holds a `T` written by
        // the installer.
        Some(unsafe { &*(start as *const T) })
    }
}

/// The generation of the SMI a CPU enters now, with the runtime block at
/// `runtime`: the owner's, if the rendezvous is taken, else the next.
#[inline(always)]
fn ingress(runtime: u64) -> u32 {
    if runtime == 0 {
        return 0;
    }
    // SAFETY: `SmmContext::from_raw`'s contract: the installer's runtime
    // block, in locked SMRAM for the whole SMI.
    let runtime = unsafe { &*(runtime as *const SmmRuntime) };
    let rendezvous = runtime.rendezvous.load(Ordering::Acquire);
    if owner_of(rendezvous) != 0 {
        generation_of(rendezvous)
    } else {
        generation_of(rendezvous).wrapping_add(1)
    }
}

/// The owner of a rendezvous value: its logical CPU index + 1, 0 if free.
#[inline(always)]
const fn owner_of(rendezvous: u64) -> u32 {
    rendezvous as u32
}

/// The generation of a rendezvous or parked value.
#[inline(always)]
const fn generation_of(value: u64) -> u32 {
    (value >> 32) as u32
}

/// The CPUs parked for `generation`.
#[inline(always)]
fn parked_for(parked: &core::sync::atomic::AtomicU64, generation: u32) -> u32 {
    let value = parked.load(Ordering::Acquire);
    if generation_of(value) == generation {
        value as u32
    } else {
        0
    }
}

/// Take the free rendezvous for `token`, in the next generation: `Ok` with
/// that generation, or `Err` with the owner's rendezvous value.
#[cfg(any(feature = "stage-bin", test))]
#[inline(always)]
fn claim(rendezvous: &core::sync::atomic::AtomicU64, token: u32) -> Result<u32, u64> {
    let mut value = rendezvous.load(Ordering::Acquire);
    loop {
        if owner_of(value) != 0 {
            return Err(value);
        }
        let generation = generation_of(value).wrapping_add(1);
        let next = u64::from(generation) << 32 | u64::from(token);
        match rendezvous.compare_exchange_weak(value, next, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => return Ok(generation),
            Err(now) => value = now,
        }
    }
}

/// This CPU is parked for `generation`'s owner. An acknowledgement of an
/// older generation (a CPU that saw that owner, which released meanwhile)
/// leaves a newer generation's count alone.
#[cfg(any(feature = "stage-bin", test))]
#[inline(always)]
fn acknowledge(parked: &core::sync::atomic::AtomicU64, generation: u32) {
    let mut value = parked.load(Ordering::Acquire);
    loop {
        let counted = generation_of(value);
        let next = if counted == generation {
            value + 1
        } else if (generation.wrapping_sub(counted) as i32) > 0 {
            u64::from(generation) << 32 | 1
        } else {
            return;
        };
        match parked.compare_exchange_weak(value, next, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => return,
            Err(now) => value = now,
        }
    }
}

/// Enter the permanent SMI rendezvous.
///
/// Exactly one CPU owns shared chipset dispatch, in a generation of its
/// own. Every other CPU acknowledges that generation (it is parked for
/// that owner, its local work done) and stays in SMM until that owner
/// releases it, then returns through RSM. This matches coreboot's
/// `smi_obtain_lock()` handling in `smm_module_handler.c`, with the
/// owner's count of parked CPUs tied to its generation
/// ([`SmmContext::cpus_held`]).
///
/// # Safety
///
/// `ctx.params.runtime` must point to a live runtime block in locked SMRAM.
#[cfg(any(feature = "stage-bin", test))]
#[inline(always)]
pub(crate) unsafe fn enter_rendezvous(ctx: &SmmContext<'_>) -> bool {
    let Some(runtime) = ctx.runtime_ptr() else {
        return false;
    };
    let (rendezvous, parked) = unsafe { (&(*runtime).rendezvous, &(*runtime).parked) };
    match claim(rendezvous, ctx.cpu().wrapping_add(1)) {
        Ok(_) => true,
        Err(owned) => {
            acknowledge(parked, generation_of(owned));
            // Released (or, if this CPU was late, already another owner's):
            // this generation is over for it.
            while rendezvous.load(Ordering::Acquire) == owned {
                unsafe { asm!("pause", options(nomem, nostack, preserves_flags)) };
            }
            false
        }
    }
}

/// Release the rendezvous taken by [`enter_rendezvous`]: its generation
/// ends, and with it what its parked CPUs acknowledged.
///
/// # Safety
///
/// The caller must own the current SMI's rendezvous, and
/// `ctx.params.runtime` must point to its live SMRAM runtime block.
#[cfg(any(feature = "stage-bin", test))]
#[inline(always)]
pub(crate) unsafe fn leave_rendezvous(ctx: &SmmContext<'_>) {
    let Some(runtime) = ctx.runtime_ptr() else {
        return;
    };
    let rendezvous = unsafe { &(*runtime).rendezvous };
    let owned = rendezvous.load(Ordering::Acquire);
    if owner_of(owned) == ctx.cpu().wrapping_add(1) {
        rendezvous.store(owned & !u64::from(u32::MAX), Ordering::Release);
    }
}

/// Emit a minimal SMM debug trace for a CPU number.
///
/// # Safety
///
/// The caller must run where `DEBUGCON` port I/O is permitted.
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[inline(always)]
pub unsafe fn debug_trace(cpu: u32) {
    unsafe {
        fstart_core::pio::outb(DEBUGCON, b'S');
        let mut digit = (cpu & 0x0f) as u8;
        if digit > 9 {
            digit = digit.wrapping_add(7);
        }
        fstart_core::pio::outb(DEBUGCON, digit.wrapping_add(b'0'));
        fstart_core::pio::outb(DEBUGCON, b'\n');
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    impl<'a> SmmContext<'a> {
        /// A CPU entering now.
        fn at(params: &'a mut SmmEntryParams) -> Self {
            unsafe { Self::from_raw(params) }.unwrap()
        }
    }

    /// A CPU's SMI is the generation it entered with: one that entered
    /// before the first owner claimed it, or while that owner held it,
    /// owns a later generation once it is released, and is not the first
    /// owner (a source that came between the owners, which it dispatches,
    /// does not make it one). A CPU that enters after the release, with
    /// every other CPU gone, is the next SMI's.
    #[test]
    fn only_the_first_owner_of_an_smi_is_its_first() {
        let mut runtime = SmmRuntime::new(0, 0, 3, 0x400, 0, 0);
        let base = core::ptr::addr_of_mut!(runtime) as u64;
        let (mut a, mut b, mut c) = (params(0, base), params(1, base), params(2, base));
        // A and B enter one SMI; A claims it.
        let a = SmmContext::at(&mut a);
        let early = SmmContext::at(&mut b);
        assert!(unsafe { enter_rendezvous(&a) });
        assert!(a.first_owner());
        // C enters late, while A holds it.
        let late = SmmContext::at(&mut c);
        unsafe { leave_rendezvous(&a) };
        // B and C each claim the rendezvous after A: neither is first.
        assert!(unsafe { enter_rendezvous(&early) });
        assert!(!early.first_owner(), "entered before A claimed");
        unsafe { leave_rendezvous(&early) };
        assert!(unsafe { enter_rendezvous(&late) });
        assert!(!late.first_owner(), "entered while A held it");
        assert_eq!(late.cpus_held(), Some((1, 3)));
        unsafe { leave_rendezvous(&late) };
        // The next SMI, entered with the rendezvous free.
        let mut next = params(1, base);
        let next = SmmContext::at(&mut next);
        assert!(unsafe { enter_rendezvous(&next) });
        assert!(next.first_owner());
        unsafe { leave_rendezvous(&next) };
        assert!(!next.first_owner(), "released");
    }

    fn params(cpu: u32, runtime: u64) -> SmmEntryParams {
        SmmEntryParams {
            cpu,
            stack_size: 0,
            stack_top: 0,
            common_entry: 0,
            runtime,
            coreboot_module_args: 0,
            cr3: 0,
            entry_base: 0,
        }
    }

    #[test]
    fn owner_enter_leave_and_failed_claim_preserve_the_token() {
        let mut runtime = SmmRuntime::new(0, 0, 1, 0x400, 0, 0);
        let mut entry = params(0, core::ptr::addr_of_mut!(runtime) as u64);
        let ctx = SmmContext::at(&mut entry);
        assert!(unsafe { enter_rendezvous(&ctx) });
        assert_eq!(runtime.rendezvous.load(Ordering::Acquire), 1 << 32 | 1);
        assert_eq!(claim(&runtime.rendezvous, 2), Err(1 << 32 | 1));
        assert_eq!(ctx.cpus_held(), Some((1, 1)));
        unsafe { leave_rendezvous(&ctx) };
        assert_eq!(runtime.rendezvous.load(Ordering::Acquire), 1 << 32);
        assert_eq!(ctx.cpus_held(), None, "not the owner any more");
    }

    /// The owner counts the CPUs parked for its generation, not the ones of
    /// the previous owner that are still on their way out: B, parked for
    /// A's first SMI, is released but has not reached its RSM when A takes
    /// the next SMI. B is not held for that one until it acknowledges it.
    #[test]
    fn a_previous_generations_waiter_is_not_held_for_the_next_owner() {
        let mut runtime = SmmRuntime::new(0, 0, 2, 0x400, 0, 0);
        let base = core::ptr::addr_of_mut!(runtime) as u64;
        let mut a = params(0, base);
        let a = SmmContext::at(&mut a);
        // A owns generation 1; B sees it, parks, and is counted.
        assert!(unsafe { enter_rendezvous(&a) });
        let first = runtime.rendezvous.load(Ordering::Acquire);
        assert_eq!(claim(&runtime.rendezvous, 2), Err(first));
        acknowledge(&runtime.parked, generation_of(first));
        assert_eq!(a.cpus_held(), Some((2, 2)));
        // A releases; B is between the release and its RSM. A's next SMI.
        unsafe { leave_rendezvous(&a) };
        assert!(unsafe { enter_rendezvous(&a) });
        assert_eq!(generation_of(runtime.rendezvous.load(Ordering::Acquire)), 2);
        assert_eq!(a.cpus_held(), Some((1, 2)), "B is not held for this owner");
        // A late acknowledgement of generation 1 does not count for 2.
        acknowledge(&runtime.parked, 1);
        assert_eq!(a.cpus_held(), Some((1, 2)));
        // B's RSM, then this SMI on B: parked for generation 2.
        let second = runtime.rendezvous.load(Ordering::Acquire);
        assert_eq!(claim(&runtime.rendezvous, 2), Err(second));
        acknowledge(&runtime.parked, generation_of(second));
        assert_eq!(a.cpus_held(), Some((2, 2)));
        // B's own context is no owner's.
        let mut b = params(1, base);
        assert_eq!(SmmContext::at(&mut b).cpus_held(), None);
        unsafe { leave_rendezvous(&a) };
    }

    /// Generations wrap: an acknowledgement of the generation before the
    /// wrap is older than the one after it.
    #[test]
    fn generations_wrap() {
        let parked = core::sync::atomic::AtomicU64::new(u64::from(u32::MAX) << 32 | 3);
        acknowledge(&parked, 0);
        assert_eq!(parked_for(&parked, 0), 1);
        acknowledge(&parked, u32::MAX);
        assert_eq!(parked_for(&parked, 0), 1);
    }

    #[repr(C, align(16))]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct Config([u64; 8]);

    #[repr(C)]
    struct Fixture {
        runtime: SmmRuntime,
        config: Config,
    }

    #[test]
    #[allow(unused_assignments)]
    fn handler_config_requires_exact_size_inside_smram() {
        let mut fixture = Fixture {
            runtime: SmmRuntime::new(0, 0, 1, 0x400, 0, 0),
            config: Config([0x600, 0x20, 2, 0, 4, 5, 6, 7]),
        };
        let base = core::ptr::addr_of!(fixture) as u64;
        let offset = core::mem::offset_of!(Fixture, config) as u32;
        fixture.runtime = SmmRuntime::new(
            base,
            size_of::<Fixture>() as u64,
            1,
            0x400,
            offset,
            size_of::<Config>() as u32,
        );
        let mut entry = params(0, base);
        let ctx = SmmContext::at(&mut entry);
        assert_eq!(
            unsafe { ctx.handler_config::<Config>() },
            Some(&Config([0x600, 0x20, 2, 0, 4, 5, 6, 7]))
        );
        assert!(core::ptr::eq(
            unsafe { ctx.handler_config::<Config>() }.unwrap(),
            &fixture.config
        ));
        assert_eq!(unsafe { ctx.handler_config::<[u16; 2]>() }, None);

        fixture.runtime.smram_size = 8;
        let ctx = SmmContext::at(&mut entry);
        assert_eq!(unsafe { ctx.handler_config::<Config>() }, None);
    }
}
