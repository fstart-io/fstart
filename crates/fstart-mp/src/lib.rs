//! x86 Multi-Processor initialization and scoped work dispatch.
//!
//! This crate brings up Application Processors (APs) on x86 via the
//! standard INIT + SIPI protocol, runs a configurable flight plan for
//! CPU and SMM initialization, then provides a scoped work dispatch
//! API modelled after [`std::thread::scope`].
//!
//! # Architecture
//!
//! ```text
//!                         ┌──────────────┐
//!                         │  mp_init()   │
//!                         └──────┬───────┘
//!                                │
//!            ┌───────────────────┼───────────────────┐
//!            │                   │                   │
//!     pre_mp_init()       copy SIPI trampoline     mirror MTRRs
//!            │                   │                   │
//!            └───────────────────┼───────────────────┘
//!                                │
//!                        send INIT + SIPI
//!                                │
//!                    ┌───────────┴────────────┐
//!                    │  Flight plan (barriered │
//!                    │  steps for BSP + APs)   │
//!                    └───────────┬────────────┘
//!                                │
//!                         APs → mailbox spin
//!                                │
//!                    ┌───────────┴────────────┐
//!                    │     MpHandle           │
//!                    │  ├── scope()           │
//!                    │  │   ├── broadcast()   │
//!                    │  │   ├── scatter()     │
//!                    │  │   └── run_on()      │
//!                    │  └── park_aps()        │
//!                    └────────────────────────┘
//! ```
//!
//! # Scoped closures
//!
//! The [`MpHandle::scope`] method provides structured concurrency:
//! closures dispatched within a scope can safely borrow from the
//! caller's stack frame, because the scope waits for all APs to
//! complete before returning.  No `alloc`, no `Box`, no `dyn` —
//! closures are type-erased via monomorphized trampolines at zero cost.
//!
//! ```ignore
//! let timing = compute_timing(&spd);
//! mp.scope(|s| {
//!     s.broadcast(&|| program_msrs(&timing));  // borrows &timing
//! });
//! // timing still valid, all CPUs configured
//! ```

#![no_std]

use core::marker::PhantomData;
use core::sync::atomic::{fence, AtomicU8, AtomicUsize, Ordering};

use fstart_lapic::Lapic;

#[cfg(not(rust_analyzer))]
mod sipi_blob {
    include!(concat!(env!("OUT_DIR"), "/sipi_trampoline.rs"));
}

#[cfg(rust_analyzer)]
mod sipi_blob {
    pub const TRAMPOLINE: &[u8] = &[];
    pub const CR3_OFFSET: usize = 0;
    pub const ENTRY_OFFSET: usize = 0;
    pub const STACK_BASE_OFFSET: usize = 0;
    pub const STACK_SIZE_OFFSET: usize = 0;
    pub const AP_COUNTER_OFFSET: usize = 0;
}

// ---------------------------------------------------------------------------
// Traits
// ---------------------------------------------------------------------------

/// Per-CPU-model initialization operations.
///
/// Implementations configure MSRs (C-states, SpeedStep, thermals, etc.).
/// Every method in this trait either runs on the BSP only or on all
/// logical CPUs — the doc comment on each method states which.
///
/// This is the Rust equivalent of coreboot's `struct cpu_driver::ops`.
pub trait CpuOps: Send + Sync {
    /// Human-readable CPU model name.
    const NAME: &'static str;

    /// Called on *every* logical CPU after AP bringup.
    ///
    /// Configure model-specific MSRs here: C-state config,
    /// SpeedStep/EIST, thermal monitoring, VMX feature control, etc.
    fn init_cpu(&self);

    /// BSP-only: called before SIPI, after LAPIC setup.
    ///
    /// Use for one-time setup: MTRR configuration, microcode discovery,
    /// or early chipset programming that must happen before APs wake.
    fn pre_mp_init(&self) {}

    /// BSP-only: called after all CPUs are initialized and APs are parked.
    ///
    /// Use for post-init validation, feature lockdown, or advertising
    /// detected capabilities.
    fn post_mp_init(&self) {}

    /// Microcode blob for this CPU model.
    ///
    /// If `Some((blob, parallel))`, microcode is loaded on all CPUs during
    /// bringup.  `parallel` indicates whether concurrent loading is safe
    /// (false on Hyper-Threading parts that share microcode update logic).
    fn microcode(&self) -> Option<(&[u8], bool)> {
        None
    }
}

/// Generic x86 CPU operations used by virtual boards until model-specific
/// MSR programming is needed.
pub struct GenericX86CpuOps;

impl CpuOps for GenericX86CpuOps {
    const NAME: &'static str = "generic-x86";

    fn init_cpu(&self) {
        fstart_log::info!("cpu: generic x86 init complete");
    }
}

/// SMM (System Management Mode) setup operations.
///
/// Provided by the chipset/northbridge driver.  Controls TSEG geometry,
/// SMM handler installation, and per-CPU SMBASE relocation.
///
/// Designed as a separate trait from [`CpuOps`] because SMM is a chipset
/// concern (NB owns TSEG/SMRAM, SB controls SMI routing), while `CpuOps`
/// is a CPU-model concern (MSRs, C-states).
///
/// When `SmmOps` is not provided to [`mp_init`], SMM flight plan steps
/// are skipped entirely.
pub trait SmmOps: Send + Sync {
    /// Return the permanent SMM region geometry.
    ///
    /// Returns `None` to disable SMM for this platform.
    fn smm_info(&self) -> Option<SmmInfo>;

    /// BSP-only: install the SMM relocation and permanent handlers.
    ///
    /// Called after APs are up but before any CPU runs `smm_relocate`.
    /// `image` is the standalone native PIC SMM image generated by xtask and
    /// embedded into the stage that requested `MpInit(smm: true)`.
    fn install_smm_handlers(
        &self,
        info: &SmmInfo,
        num_cpus: u16,
        image: &[u8],
    ) -> Result<(), SmmError>;

    /// Per-CPU: trigger SMM entry to relocate this CPU's SMBASE.
    ///
    /// Typically sends a self-SMI.  On the BSP, called during
    /// `pre_smm_init`.  On APs, called as a parallel flight plan step.
    fn smm_relocate(&self);

    /// BSP-only: called after handlers are loaded, before per-CPU relocation.
    ///
    /// Use for SMRR enable, IA32_FEATURE_CONTROL setup.
    fn pre_smm_init(&self) {}

    /// BSP-only: called after all CPUs have been relocated.
    ///
    /// Use for `global_smi_enable()` and `smm_lock()`.
    fn post_smm_init(&self) {}
}

/// SMM setup error reported by chipset-specific [`SmmOps`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmmError {
    /// Native SMM image installation failed.
    InstallFailed,
}

/// SMM region geometry.
#[derive(Debug, Clone, Copy)]
pub struct SmmInfo {
    /// Base address of permanent SMRAM (TSEG).
    pub smbase: u64,
    /// Size of the permanent SMM handler region.
    pub smsize: usize,
    /// Per-CPU SMM save state area size.
    pub save_state_size: usize,
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// MP initialization configuration.
///
/// Generic over [`CpuOps`] (required).  Optionally accepts [`SmmOps`]
/// (when SMM is needed).  When `smm` is `None`, the SMM flight plan
/// steps are skipped entirely.
pub struct MpConfig<'a, C: CpuOps> {
    /// Per-CPU-model operations (MSR programming, etc.).
    pub cpu_ops: &'a C,
    /// Chipset SMM operations.  `None` = no SMM.
    pub smm: Option<&'a dyn SmmOps>,
    /// Standalone native PIC SMM image to install when `smm` is `Some`.
    pub smm_image: Option<&'a [u8]>,
    /// Total logical CPU count (BSP + APs).
    pub num_cpus: u16,
}

/// Errors from MP initialization.
#[derive(Debug)]
pub enum MpError {
    /// No APs responded to INIT+SIPI within the timeout.
    NoApsResponded,
    /// Fewer APs than expected checked in.
    PartialBringup { expected: u16, actual: u16 },
    /// SIPI trampoline placement failed.
    TrampolinePlacementFailed,
    /// SMM was requested but no SMM image was embedded/provided.
    MissingSmmImage,
    /// Chipset-specific SMM handler installation failed.
    SmmInstallFailed,
}

// ---------------------------------------------------------------------------
// Mailbox — per-AP communication slot
// ---------------------------------------------------------------------------

/// Mailbox states.
const MB_IDLE: usize = 0;

/// Per-AP mailbox for work dispatch.  Cache-line aligned to prevent
/// false sharing between adjacent mailboxes on different CPUs.
#[repr(C, align(64))]
struct ApMailbox {
    /// 0 = idle.  Non-zero = trampoline function pointer ("go" signal).
    func: AtomicUsize,
    /// Raw data pointer (argument to trampoline).
    data: AtomicUsize,
    /// CPU index assigned to this AP (1-based; 0 = BSP).
    cpu_index: AtomicUsize,
}

impl ApMailbox {
    const fn new() -> Self {
        Self {
            func: AtomicUsize::new(MB_IDLE),
            data: AtomicUsize::new(0),
            cpu_index: AtomicUsize::new(0),
        }
    }
}

/// Maximum number of CPUs supported.  Determines static mailbox array size.
const MAX_CPUS: usize = 64;

/// Static mailbox array.  One slot per AP (index 0 = AP #1, etc.).
/// Placed in BSS (zero-init = idle).
static MAILBOXES: [ApMailbox; MAX_CPUS] = {
    // const-init workaround: can't use array::from_fn in const
    const MB: ApMailbox = ApMailbox::new();
    [MB; MAX_CPUS]
};

const AP_STACK_SIZE: usize = 16 * 1024;

#[repr(C, align(16))]
struct ApStacks([[u8; AP_STACK_SIZE]; MAX_CPUS]);

static mut AP_STACKS: ApStacks = ApStacks([[0; AP_STACK_SIZE]; MAX_CPUS]);

/// Atomic counter: number of APs that have checked in.
static AP_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Flag: tells APs to enter the mailbox loop (vs flight plan).
static AP_IN_MAILBOX_LOOP: AtomicU8 = AtomicU8::new(0);

// ---------------------------------------------------------------------------
// Flight plan (internal)
// ---------------------------------------------------------------------------

/// Maximum flight plan steps.
const MAX_FLIGHT_STEPS: usize = 8;

/// A single step in the flight plan.
///
/// APs increment `cpus_entered`, then wait on `barrier`.  The BSP
/// waits for all APs to enter, calls `bsp_call`, then releases the
/// barrier.  If `barrier` starts at 1, APs proceed immediately
/// (no-block mode, used for parallel SMM relocation).
struct FlightStep {
    /// 0 = APs blocked, 1 = APs may proceed.
    barrier: AtomicUsize,
    /// Number of APs that have reached this step.
    cpus_entered: AtomicUsize,
    /// Function for APs to call (0 = skip).
    ap_fn: AtomicUsize,
    /// Function for BSP to call (0 = skip).
    bsp_fn: AtomicUsize,
}

impl FlightStep {
    const fn blocked(ap: usize, bsp: usize) -> Self {
        Self {
            barrier: AtomicUsize::new(0),
            cpus_entered: AtomicUsize::new(0),
            ap_fn: AtomicUsize::new(ap),
            bsp_fn: AtomicUsize::new(bsp),
        }
    }
    const fn empty() -> Self {
        Self::blocked(0, 0)
    }
}

/// Global flight plan.  Set by BSP before APs are released.
static FLIGHT_PLAN: [FlightStep; MAX_FLIGHT_STEPS] = {
    const STEP: FlightStep = FlightStep::empty();
    [STEP; MAX_FLIGHT_STEPS]
};
static FLIGHT_PLAN_LEN: AtomicUsize = AtomicUsize::new(0);

// ---------------------------------------------------------------------------
// Trampoline types used by the flight plan
// ---------------------------------------------------------------------------

/// fn() signature for flight plan callbacks.
type FlightFn = fn();

/// Global CpuOps pointer — stores a `*const C` set by BSP before APs start.
static CPU_OPS_PTR: AtomicUsize = AtomicUsize::new(0);
/// Global cpu_init trampoline — monomorphized fn() that reads CPU_OPS_PTR.
static CPU_INIT_FN: AtomicUsize = AtomicUsize::new(0);
/// Global `&dyn SmmOps` fat pointer split into data/vtable words for
/// monomorphized `fn()` flight-plan callbacks.
static SMM_OPS_DATA: AtomicUsize = AtomicUsize::new(0);
static SMM_OPS_VTABLE: AtomicUsize = AtomicUsize::new(0);
/// Global smm_relocate trampoline.
static SMM_RELOCATE_FN: AtomicUsize = AtomicUsize::new(0);

/// Build a monomorphized cpu_init trampoline for concrete `CpuOps` type.
///
/// Returns a `fn()` that loads `CPU_OPS_PTR`, casts to `&C`, and calls
/// `C::init_cpu()`. This avoids the recursion bug where `flight_cpu_init`
/// would call itself through `CPU_INIT_FN`.
fn make_cpu_init_trampoline<C: CpuOps>() -> fn() {
    fn trampoline<C: CpuOps>() {
        let ptr = CPU_OPS_PTR.load(Ordering::Acquire);
        if ptr != 0 {
            // SAFETY: ptr was set by BSP to a valid &C before APs started.
            let ops: &C = unsafe { &*(ptr as *const C) };
            ops.init_cpu();
        }
    }
    trampoline::<C>
}

fn store_smm_ops(ops: &dyn SmmOps) {
    // SAFETY: A trait-object reference is two machine words on this target
    // family (data pointer + vtable pointer).  APs use it only while
    // `mp_init()` is active and the borrowed platform object is still alive.
    let (data, vtable): (usize, usize) = unsafe { core::mem::transmute(ops) };
    SMM_OPS_DATA.store(data, Ordering::Release);
    SMM_OPS_VTABLE.store(vtable, Ordering::Release);
}

fn clear_smm_ops() {
    SMM_OPS_DATA.store(0, Ordering::Release);
    SMM_OPS_VTABLE.store(0, Ordering::Release);
}

fn load_smm_ops() -> Option<&'static dyn SmmOps> {
    let data = SMM_OPS_DATA.load(Ordering::Acquire);
    let vtable = SMM_OPS_VTABLE.load(Ordering::Acquire);
    if data == 0 || vtable == 0 {
        return None;
    }
    // SAFETY: set by `store_smm_ops()` before the flight plan is released;
    // both words remain valid until the BSP clears them after SMM init.
    Some(unsafe { core::mem::transmute((data, vtable)) })
}

fn smm_relocate_trampoline() {
    if let Some(ops) = load_smm_ops() {
        ops.smm_relocate();
    }
}

fn smm_post_init_trampoline() {
    if let Some(ops) = load_smm_ops() {
        ops.post_smm_init();
    }
}

/// AP mailbox loop — the terminal flight plan step for APs.
fn ap_mailbox_loop() {
    AP_IN_MAILBOX_LOOP.store(1, Ordering::Release);

    // Determine our CPU index from the AP counter.
    // (Each AP atomically claimed an index during bringup.)
    // We find our mailbox by reading the cpu_index stored in each slot.
    let my_index = current_cpu_index();
    if my_index == 0 || my_index as usize > MAX_CPUS {
        // BSP or invalid — shouldn't be in the mailbox loop.
        return;
    }
    let mb = &MAILBOXES[my_index as usize - 1]; // AP indices are 1-based

    loop {
        let func_ptr = mb.func.load(Ordering::Acquire);
        if func_ptr == MB_IDLE {
            core::hint::spin_loop();
            continue;
        }

        // Read the data pointer (guaranteed visible by Acquire on func).
        let data_ptr = mb.data.load(Ordering::Relaxed);

        // Call the trampoline.
        // SAFETY: BSP wrote a valid trampoline fn pointer and data
        // pointer.  The scope guarantees the data is alive.
        let trampoline: fn(*const (), u32) = unsafe { core::mem::transmute(func_ptr) };
        trampoline(data_ptr as *const (), my_index);

        // Signal completion by clearing the func slot.
        mb.func.store(MB_IDLE, Ordering::Release);
    }
}

/// Read the current CPU's logical index from a thread-local variable.
///
/// During AP bringup, each AP stores its index. The BSP is always 0.
/// For now, we use the LAPIC ID as a proxy and map it via the AP
/// assignment order.
fn current_cpu_index() -> u32 {
    let lapic = Lapic::from_msr();
    let id = lapic.id();
    // Search mailboxes for our LAPIC ID.
    // During bringup, we store the LAPIC ID in cpu_index.
    // BSP always has index 0.
    if Lapic::is_bsp() {
        return 0;
    }
    for i in 0..MAX_CPUS {
        if MAILBOXES[i].cpu_index.load(Ordering::Relaxed) == id as usize {
            return (i + 1) as u32; // 1-based AP index
        }
    }
    // Fallback: use raw LAPIC ID (not ideal but won't crash).
    id
}

// ---------------------------------------------------------------------------
// AP entry point (called from SIPI trampoline)
// ---------------------------------------------------------------------------

/// C-callable AP entry point.  The SIPI trampoline jumps here after
/// entering 64-bit long mode with a valid stack.
///
/// `index` is the 0-based AP ordinal (first AP to check in = 0, etc.).
///
/// # Safety
///
/// Called from assembly with a valid stack and identity-mapped page tables.
#[no_mangle]
pub extern "C" fn fstart_ap_entry(index: u32) -> ! {
    // Enable the local APIC and set up virtual wire.
    let lapic = Lapic::from_msr();
    lapic.enable();
    lapic.setup_virtual_wire(false);

    // Store our LAPIC ID in the mailbox so BSP can identify us.
    let ap_slot = index as usize;
    if ap_slot < MAX_CPUS {
        MAILBOXES[ap_slot]
            .cpu_index
            .store(lapic.id() as usize, Ordering::Release);
    }

    // Increment the global AP counter — BSP is waiting for this.
    AP_COUNT.fetch_add(1, Ordering::Release);

    // Walk the flight plan.
    let num_steps = FLIGHT_PLAN_LEN.load(Ordering::Acquire);
    for i in 0..num_steps {
        let step = &FLIGHT_PLAN[i];

        // Signal that we've reached this step.
        step.cpus_entered.fetch_add(1, Ordering::Release);

        // Wait for the barrier (BSP releases it after all APs check in).
        while step.barrier.load(Ordering::Acquire) == 0 {
            core::hint::spin_loop();
        }

        // Call the AP function if present.
        let ap_fn = step.ap_fn.load(Ordering::Acquire);
        if ap_fn != 0 {
            // SAFETY: BSP set this to a valid fn() before releasing APs.
            let f: FlightFn = unsafe { core::mem::transmute(ap_fn) };
            f();
        }
    }

    // If we get past the flight plan, enter the mailbox loop.
    ap_mailbox_loop();

    // Should never reach here.
    loop {
        // SAFETY: HLT is always safe — just stops until next interrupt.
        unsafe { core::arch::asm!("hlt", options(nomem, nostack)) };
    }
}

// ---------------------------------------------------------------------------
// mp_init — the main entry point
// ---------------------------------------------------------------------------

/// Initialize all CPUs.
///
/// Brings up application processors via INIT+SIPI, runs a flight plan
/// for CPU and optional SMM initialization, then parks APs in a
/// mailbox loop for later work dispatch via [`MpHandle::scope`].
///
/// # Sequence
///
/// 1. BSP: enable LAPIC, call `cpu_ops.pre_mp_init()`
/// 2. BSP: copy SIPI trampoline to low memory (`0x1000`)
/// 3. BSP: send INIT + SIPI to all APs
/// 4. BSP: wait for APs to check in (with timeout)
/// 5. Flight plan:
///    - If SMM: step "install handlers" (BSP), step "relocate" (all, parallel)
///    - Step "cpu_init" (all CPUs, barriered)
///    - Step "mailbox loop" (APs park, BSP continues)
/// 6. BSP: `smm.post_smm_init()` (if SMM), `cpu_ops.post_mp_init()`
/// 7. Return [`MpHandle`]
pub fn mp_init<C: CpuOps>(config: &MpConfig<'_, C>) -> Result<MpHandle, MpError> {
    let num_aps = config.num_cpus.saturating_sub(1);

    fstart_log::info!("mp: initializing {} CPUs ({})", config.num_cpus, C::NAME);

    // --- Step 1: BSP LAPIC setup ---
    let lapic = Lapic::from_msr();
    lapic.enable();
    lapic.setup_virtual_wire(true);

    fstart_log::info!("mp: BSP LAPIC ID = {}", lapic.id());

    // Pre-MP init (BSP only).
    config.cpu_ops.pre_mp_init();

    if num_aps == 0 {
        // Single-CPU system.  Still perform the SMM install + relocation path
        // when requested; coreboot also relocates the BSP before enabling
        // global SMIs.
        if let Some(smm) = config.smm {
            if let Some(info) = smm.smm_info() {
                smm.pre_smm_init();
                let image = config.smm_image.ok_or_else(|| {
                    fstart_log::error!("mp: SMM requested but no SMM image was provided");
                    MpError::MissingSmmImage
                })?;
                smm.install_smm_handlers(&info, config.num_cpus, image)
                    .map_err(|_| MpError::SmmInstallFailed)?;
                // First SMI runs the default-SMRAM relocation handler; after
                // post_smm_init() closes/locks SMRAM, the second SMI proves the
                // permanent copied handler is usable.
                smm.smm_relocate();
                smm.post_smm_init();
                smm.smm_relocate();
            }
        }
        config.cpu_ops.init_cpu();
        config.cpu_ops.post_mp_init();
        return Ok(MpHandle { num_aps: 0 });
    }

    // --- Step 2: Set up global state for APs ---
    AP_COUNT.store(0, Ordering::Release);
    AP_IN_MAILBOX_LOOP.store(0, Ordering::Release);

    // Store ops pointer globally so the monomorphized trampoline can access it.
    CPU_OPS_PTR.store(config.cpu_ops as *const C as usize, Ordering::Release);
    let cpu_init_trampoline = make_cpu_init_trampoline::<C>();

    // Build the flight plan.
    let mut step_count = 0usize;
    let smm_info = config.smm.and_then(|smm| {
        let info = smm.smm_info();
        if info.is_some() {
            store_smm_ops(smm);
            SMM_RELOCATE_FN.store(
                smm_relocate_trampoline as *const () as usize,
                Ordering::Release,
            );
        }
        info
    });

    // If SMM is configured, APs first block at a relocation step.  The BSP
    // installs the handlers after AP check-in and before releasing this step.
    // A later BSP-only post step closes/locks SMRAM and enables global SMI;
    // then every CPU triggers one more SMI through the permanent handler so
    // multi-core SMM entry is validated before APs park in the mailbox loop.
    if smm_info.is_some() {
        FLIGHT_PLAN[step_count].barrier.store(0, Ordering::Release);
        FLIGHT_PLAN[step_count]
            .cpus_entered
            .store(0, Ordering::Release);
        FLIGHT_PLAN[step_count].ap_fn.store(
            smm_relocate_trampoline as *const () as usize,
            Ordering::Release,
        );
        FLIGHT_PLAN[step_count].bsp_fn.store(
            smm_relocate_trampoline as *const () as usize,
            Ordering::Release,
        );
        step_count += 1;

        FLIGHT_PLAN[step_count].barrier.store(0, Ordering::Release);
        FLIGHT_PLAN[step_count]
            .cpus_entered
            .store(0, Ordering::Release);
        FLIGHT_PLAN[step_count].ap_fn.store(0, Ordering::Release);
        FLIGHT_PLAN[step_count].bsp_fn.store(
            smm_post_init_trampoline as *const () as usize,
            Ordering::Release,
        );
        step_count += 1;

        FLIGHT_PLAN[step_count].barrier.store(0, Ordering::Release);
        FLIGHT_PLAN[step_count]
            .cpus_entered
            .store(0, Ordering::Release);
        FLIGHT_PLAN[step_count].ap_fn.store(
            smm_relocate_trampoline as *const () as usize,
            Ordering::Release,
        );
        FLIGHT_PLAN[step_count].bsp_fn.store(
            smm_relocate_trampoline as *const () as usize,
            Ordering::Release,
        );
        step_count += 1;
    }

    // Step: All CPUs run cpu_init (barriered).
    CPU_INIT_FN.store(cpu_init_trampoline as usize, Ordering::Release);
    FLIGHT_PLAN[step_count].barrier.store(0, Ordering::Release);
    FLIGHT_PLAN[step_count]
        .cpus_entered
        .store(0, Ordering::Release);
    FLIGHT_PLAN[step_count]
        .ap_fn
        .store(cpu_init_trampoline as usize, Ordering::Release);
    FLIGHT_PLAN[step_count]
        .bsp_fn
        .store(cpu_init_trampoline as usize, Ordering::Release);
    step_count += 1;

    // Step: APs enter mailbox loop (barriered).
    FLIGHT_PLAN[step_count].barrier.store(0, Ordering::Release);
    FLIGHT_PLAN[step_count]
        .cpus_entered
        .store(0, Ordering::Release);
    FLIGHT_PLAN[step_count]
        .ap_fn
        .store(ap_mailbox_loop as *const () as usize, Ordering::Release);
    FLIGHT_PLAN[step_count].bsp_fn.store(0, Ordering::Release);
    step_count += 1;

    FLIGHT_PLAN_LEN.store(step_count, Ordering::Release);
    fence(Ordering::SeqCst);

    // --- Step 3: Copy SIPI trampoline to low memory ---
    // The trampoline will be defined in sipi.rs (global_asm!).
    // For now, we set up the parameter block and copy.
    install_sipi_trampoline(num_aps, &lapic)?;

    // --- Step 4: Send INIT + SIPI ---
    fstart_log::info!("mp: sending INIT IPI");
    lapic.send_init_all_but_self();

    // Wait 10 ms after INIT (Intel SDM requirement).
    delay_us(10_000);

    // First SIPI.
    fstart_log::info!("mp: sending SIPI (vector page {:#x})", SIPI_VECTOR_PAGE);
    lapic.send_sipi_all_but_self(SIPI_VECTOR_PAGE as u8);
    delay_us(200);

    // Check if all APs responded.
    let checked_in = AP_COUNT.load(Ordering::Acquire) as u16;
    if checked_in < num_aps {
        // Second SIPI (per Intel SDM recommendation).
        lapic.send_sipi_all_but_self(SIPI_VECTOR_PAGE as u8);
        // Wait up to 50 ms per AP.
        let timeout_us = 50_000u64 * num_aps as u64;
        let mut elapsed = 0u64;
        while (AP_COUNT.load(Ordering::Acquire) as u16) < num_aps && elapsed < timeout_us {
            delay_us(50);
            elapsed += 50;
        }
    }

    let final_count = AP_COUNT.load(Ordering::Acquire) as u16;
    fstart_log::info!("mp: {}/{} APs checked in", final_count, num_aps);

    if final_count == 0 {
        return Err(MpError::NoApsResponded);
    }

    // Install SMM handlers only after APs have checked in and are blocked at
    // the first flight-plan step.  This matches coreboot's sequencing: load
    // permanent handlers, then let every CPU enter SMM to relocate SMBASE.
    if let (Some(smm), Some(info)) = (config.smm, smm_info) {
        smm.pre_smm_init();
        let image = config.smm_image.ok_or_else(|| {
            fstart_log::error!("mp: SMM requested but no SMM image was provided");
            MpError::MissingSmmImage
        })?;
        smm.install_smm_handlers(&info, config.num_cpus, image)
            .map_err(|_| MpError::SmmInstallFailed)?;
    }

    // --- Step 5: Walk the flight plan (BSP side) ---
    for i in 0..step_count {
        let step = &FLIGHT_PLAN[i];

        // Wait for all APs to reach this step (if barrier is 0 = blocked).
        if step.barrier.load(Ordering::Acquire) == 0 {
            let timeout_us = 1_000_000u64; // 1 second
            let mut elapsed = 0u64;
            while (step.cpus_entered.load(Ordering::Acquire) as u16) < final_count {
                delay_us(100);
                elapsed += 100;
                if elapsed >= timeout_us {
                    fstart_log::error!("mp: flight plan step {} timeout", i);
                    break;
                }
            }
        }

        // BSP calls its function.
        let bsp_fn = step.bsp_fn.load(Ordering::Acquire);
        if bsp_fn != 0 {
            // SAFETY: we set this to a valid fn() above.
            let f: FlightFn = unsafe { core::mem::transmute(bsp_fn) };
            f();
        }

        // Release the barrier so APs can proceed.
        step.barrier.store(1, Ordering::Release);
    }

    // --- Step 6: Post-init ---
    clear_smm_ops();
    config.cpu_ops.post_mp_init();

    fstart_log::info!("mp: initialization complete ({} CPUs)", final_count + 1);

    if final_count < num_aps {
        return Err(MpError::PartialBringup {
            expected: num_aps,
            actual: final_count,
        });
    }

    Ok(MpHandle { num_aps })
}

// ---------------------------------------------------------------------------
// SIPI trampoline installation
// ---------------------------------------------------------------------------

/// Physical page number for the SIPI vector (0x8000 = page 8).
///
/// This stays below conventional memory and avoids both the real-mode IVT/BDA
/// and the default SMRAM area at 0x30000.  Q35 boards use 0x1000 upward for
/// early page tables, so do not place the SIPI trampoline at page 1.
const SIPI_VECTOR_PAGE: u32 = 0x08;
/// Physical address of the SIPI trampoline.
const SIPI_VECTOR_ADDR: usize = (SIPI_VECTOR_PAGE as usize) << 12;

/// Install the SIPI trampoline at the vector address.
///
/// Copies the trampoline code to `SIPI_VECTOR_ADDR` and patches the
/// parameter block (GDT, stack, CR3, AP entry point, etc.).
fn install_sipi_trampoline(num_aps: u16, _lapic: &Lapic) -> Result<(), MpError> {
    if num_aps as usize > MAX_CPUS || sipi_blob::TRAMPOLINE.len() > 4096 {
        return Err(MpError::TrampolinePlacementFailed);
    }

    let dst = SIPI_VECTOR_ADDR as *mut u8;
    // SAFETY: we only expose the raw stack arena address to AP startup code;
    // Rust never creates references to individual AP stacks while they run.
    let stack_base = unsafe { core::ptr::addr_of_mut!(AP_STACKS.0) as u64 };
    // SAFETY: SIPI_VECTOR_ADDR is a conventional-memory page reserved for AP
    // startup.  The copied blob is less than one page and all patch offsets are
    // emitted by the build script from symbols inside that blob.
    unsafe {
        core::ptr::copy_nonoverlapping(
            sipi_blob::TRAMPOLINE.as_ptr(),
            dst,
            sipi_blob::TRAMPOLINE.len(),
        );

        patch_u64(dst, sipi_blob::CR3_OFFSET, read_cr3());
        patch_u64(
            dst,
            sipi_blob::ENTRY_OFFSET,
            fstart_ap_entry as *const () as usize as u64,
        );
        patch_u64(dst, sipi_blob::STACK_BASE_OFFSET, stack_base);
        patch_u32(dst, sipi_blob::STACK_SIZE_OFFSET, AP_STACK_SIZE as u32);
        patch_u32(dst, sipi_blob::AP_COUNTER_OFFSET, 0);
    }

    fstart_log::info!(
        "mp: SIPI trampoline at {:#x}, AP stacks at {:#x}, {} bytes each",
        SIPI_VECTOR_ADDR,
        stack_base as usize,
        AP_STACK_SIZE
    );
    Ok(())
}

unsafe fn patch_u32(base: *mut u8, offset: usize, value: u32) {
    // SAFETY: caller guarantees that `base + offset..+4` is inside the copied
    // SIPI trampoline page.
    unsafe { core::ptr::write_unaligned(base.add(offset) as *mut u32, value) };
}

unsafe fn patch_u64(base: *mut u8, offset: usize, value: u64) {
    // SAFETY: caller guarantees that `base + offset..+8` is inside the copied
    // SIPI trampoline page.
    unsafe { core::ptr::write_unaligned(base.add(offset) as *mut u64, value) };
}

fn read_cr3() -> u64 {
    let value: u64;
    // SAFETY: reading CR3 is side-effect-free and needed to let APs enter the
    // same identity-mapped long-mode address space as the BSP.
    unsafe {
        core::arch::asm!("mov {}, cr3", out(reg) value, options(nomem, nostack, preserves_flags))
    };
    value
}

// ---------------------------------------------------------------------------
// MpHandle — post-init work dispatch
// ---------------------------------------------------------------------------

/// Handle to the initialized MP subsystem.
///
/// Returned by [`mp_init`].  Provides scoped work dispatch to parked
/// APs and parking/shutdown operations.
///
/// The handle is `!Send` because it should only be used from the BSP.
pub struct MpHandle {
    num_aps: u16,
}

impl MpHandle {
    /// Number of application processors (excluding BSP).
    pub fn num_aps(&self) -> u16 {
        self.num_aps
    }

    /// Total CPU count (BSP + APs).
    pub fn num_cpus(&self) -> u16 {
        self.num_aps + 1
    }

    /// Structured concurrent execution across all CPUs.
    ///
    /// The closure `f` receives a [`Scope`] through which work can be
    /// dispatched to APs.  All dispatched work completes before `scope`
    /// returns — closures passed to the scope can safely borrow from
    /// the caller's stack frame.
    ///
    /// This is the firmware equivalent of [`std::thread::scope`].
    ///
    /// ```ignore
    /// let timing = compute_timing(&spd);
    /// mp.scope(|s| {
    ///     s.broadcast(&|| program_msrs(&timing));
    /// });
    /// // timing still valid
    /// ```
    pub fn scope<'env, F, R>(&self, f: F) -> R
    where
        F: for<'scope> FnOnce(&'scope Scope<'scope, 'env>) -> R,
    {
        let scope = Scope {
            handle: self,
            _scope: PhantomData,
            _env: PhantomData,
        };
        f(&scope)
    }

    /// Park all APs in a HLT loop.
    ///
    /// After this call, APs will not respond to mailbox dispatch.
    /// Call this before jumping to the payload/OS.
    pub fn park_aps(&self) {
        // Dispatch HLT to every AP.
        for i in 0..self.num_aps as usize {
            let mb = &MAILBOXES[i];
            mb.data.store(0, Ordering::Relaxed);
            mb.func
                .store(park_cpu as *const () as usize, Ordering::Release);
        }
        // Wait for all APs to pick up the park command.
        // (They won't signal completion — they're halted.)
        delay_us(1000);
        fstart_log::info!("mp: {} APs parked", self.num_aps);
    }
}

/// HLT loop for parking an AP.
fn park_cpu(_data: *const (), _cpu: u32) {
    loop {
        // SAFETY: HLT is always safe.
        unsafe { core::arch::asm!("hlt", options(nomem, nostack)) };
    }
}

// ---------------------------------------------------------------------------
// Scope — scoped work dispatch
// ---------------------------------------------------------------------------

/// A scope for dispatching work to APs.
///
/// Created by [`MpHandle::scope`].  Work closures can borrow anything
/// that lives at least as long as the `scope()` call — i.e. the
/// caller's local variables.  The scope enforces that all APs finish
/// before each dispatch method returns.
///
/// The two lifetime parameters mirror [`std::thread::Scope`]:
/// - `'scope`: the scope itself (cannot be stored beyond `scope()`)
/// - `'env`: things borrowed by work closures (outlives `'scope`)
pub struct Scope<'scope, 'env: 'scope> {
    handle: &'scope MpHandle,
    _scope: PhantomData<&'scope ()>,
    _env: PhantomData<&'env ()>,
}

impl<'scope, 'env> Scope<'scope, 'env> {
    /// Run the same closure on all APs.  BSP waits for all to complete.
    ///
    /// The closure runs independently on each AP.  It can borrow from
    /// the enclosing scope.  For CPU-indexed work, use [`scatter`](Self::scatter).
    ///
    /// `f` is `&F` (shared reference) because multiple APs call it.
    pub fn broadcast<F>(&self, f: &F)
    where
        F: Fn() + Send + Sync + 'env,
    {
        if self.handle.num_aps == 0 {
            return;
        }
        self.dispatch_all(trampoline_void::<F>, f as *const F as usize);
    }

    /// Run a closure on all CPUs (BSP + APs) with the CPU index.
    ///
    /// Each CPU receives its logical index: 0 = BSP, 1..N = APs.
    /// Use for data-parallel work with per-CPU result slots.
    ///
    /// **Ordering**: APs execute and complete *before* the BSP runs
    /// `f(0)`. This is intentional — it guarantees the BSP can safely
    /// read AP results without data races (e.g., collecting per-CPU
    /// microcode versions or cache topology into a shared array).
    ///
    /// ```ignore
    /// let results = [AtomicU32::new(0); MAX_CPUS];
    /// mp.scope(|s| {
    ///     s.scatter(&|cpu: u32| {
    ///         results[cpu as usize].store(compute(cpu), Ordering::Release);
    ///     });
    /// });
    /// ```
    pub fn scatter<F>(&self, f: &F)
    where
        F: Fn(u32) + Send + Sync + 'env,
    {
        // Dispatch to APs.
        if self.handle.num_aps > 0 {
            self.dispatch_all(trampoline_indexed::<F>, f as *const F as usize);
        }

        // BSP runs with index 0.
        f(0);

        // Wait for APs (dispatch_all already waited, but if we added
        // the BSP call after dispatch, we need to re-check).
        self.wait_all();
    }

    /// Run a closure on one specific AP.  BSP waits for it to complete.
    pub fn run_on<F>(&self, ap: u16, f: &F)
    where
        F: Fn() + Send + Sync + 'env,
    {
        if ap == 0 || ap > self.handle.num_aps {
            return;
        }
        let mb = &MAILBOXES[ap as usize - 1];
        mb.data.store(f as *const F as usize, Ordering::Relaxed);
        fence(Ordering::Release);
        mb.func.store(
            trampoline_void::<F> as *const () as usize,
            Ordering::Release,
        );

        // Wait for this AP to complete.
        while mb.func.load(Ordering::Acquire) != MB_IDLE {
            core::hint::spin_loop();
        }
    }

    /// Dispatch to all APs and wait for completion.
    fn dispatch_all(&self, trampoline: fn(*const (), u32), data: usize) {
        for i in 0..self.handle.num_aps as usize {
            let mb = &MAILBOXES[i];
            mb.data.store(data, Ordering::Relaxed);
        }
        fence(Ordering::Release);
        for i in 0..self.handle.num_aps as usize {
            let mb = &MAILBOXES[i];
            mb.func
                .store(trampoline as *const () as usize, Ordering::Release);
        }
        self.wait_all();
    }

    /// Wait for all APs to return to idle.
    fn wait_all(&self) {
        for i in 0..self.handle.num_aps as usize {
            while MAILBOXES[i].func.load(Ordering::Acquire) != MB_IDLE {
                core::hint::spin_loop();
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Trampolines — monomorphized closure-to-fn-ptr adapters
// ---------------------------------------------------------------------------

/// Trampoline for `Fn()` closures (ignores cpu index).
fn trampoline_void<F: Fn()>(data: *const (), _cpu: u32) {
    // SAFETY: `data` points to a valid `F` on the BSP's stack.
    // The scope guarantees `F` outlives this call.
    let f = unsafe { &*(data as *const F) };
    f();
}

/// Trampoline for `Fn(u32)` closures (passes cpu index).
fn trampoline_indexed<F: Fn(u32)>(data: *const (), cpu: u32) {
    // SAFETY: `data` points to a valid `F` on the BSP's stack.
    // The scope guarantees `F` outlives this call.
    let f = unsafe { &*(data as *const F) };
    f(cpu);
}

// ---------------------------------------------------------------------------
// Delay helper
// ---------------------------------------------------------------------------

/// Spin-delay for approximately `us` microseconds.
///
/// Uses a simple calibrated loop.  On x86, each iteration of PAUSE is
/// ~10-140 cycles depending on the microarchitecture.  We assume a
/// conservative 100 ns per PAUSE (10 MHz effective).
fn delay_us(us: u64) {
    // Rough approximation: 10 PAUSE iterations per microsecond at ~1 GHz.
    // This is intentionally conservative.  For precise timing, use
    // the LAPIC timer or TSC.
    let iterations = us * 10;
    for _ in 0..iterations {
        core::hint::spin_loop();
    }
}
