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
//!     pre_mp_init()       copy SIPI trampoline     publish MP state
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

#![allow(
    clippy::declare_interior_mutable_const,
    clippy::doc_lazy_continuation,
    clippy::missing_transmute_annotations,
    clippy::needless_range_loop
)]

use core::marker::PhantomData;
use core::sync::atomic::{AtomicPtr, AtomicU8, AtomicUsize, Ordering, fence};

use crate::x86::lapic::Lapic;
use zerocopy::IntoBytes;

pub mod cache;
mod exception;

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
    pub const AP_LIMIT_OFFSET: usize = 0;
    pub const AP_COUNTER_OFFSET: usize = 0;
    pub const MSR_COUNT_OFFSET: usize = 0;
    pub const MSR_TABLE_OFFSET: usize = 0;
    pub const CACHE_READY_OFFSET: usize = 0;
    pub const CACHE_DONE_OFFSET: usize = 0;
    pub const CACHE_RELEASE_OFFSET: usize = 0;
    pub const BSP_CACHE_OFF_OFFSET: usize = 0;
    pub const CACHE_COORDINATED_OFFSET: usize = 0;
    pub const IDTR32_OFFSET: usize = 0;
    pub const IDTR64_OFFSET: usize = 0;
    pub const FAULT32_OFFSET: usize = 0;
    pub const FAULT64_OFFSET: usize = 0;
}

// ---------------------------------------------------------------------------
// Traits
// ---------------------------------------------------------------------------

/// x86 CPU vendor identified by CPUID leaf 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpuVendor {
    /// GenuineIntel.
    Intel,
    /// AuthenticAMD.
    Amd,
    /// Any other vendor string.
    Other,
}

/// CPU identity used to select a model driver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuIdentity {
    /// CPUID vendor.
    pub vendor: CpuVendor,
    /// CPUID.1:EAX family/model/stepping signature.
    pub signature: u32,
}

impl CpuIdentity {
    /// Identify the currently running x86 CPU.
    pub fn current() -> Self {
        let (_, ebx, ecx, edx) = crate::x86::cpuid(0);
        let vendor = if ebx == 0x756e_6547 && edx == 0x4965_6e69 && ecx == 0x6c65_746e {
            CpuVendor::Intel
        } else if ebx == 0x6874_7541 && edx == 0x6974_6e65 && ecx == 0x444d_4163 {
            CpuVendor::Amd
        } else {
            CpuVendor::Other
        };
        let (signature, _, _, _) = crate::x86::cpuid(1);
        Self { vendor, signature }
    }

    /// CPUID family value with extended family folded in.
    pub fn family(self) -> u32 {
        ((self.signature >> 8) & 0x0f) + ((self.signature >> 20) & 0xff)
    }

    /// CPUID model value with extended model folded in.
    pub fn model(self) -> u32 {
        ((self.signature >> 4) & 0x0f) + ((self.signature >> 12) & 0xf0)
    }
}

/// One CPUID match entry for a CPU model driver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuIdMatch {
    /// Required vendor.
    pub vendor: CpuVendor,
    /// Signature value after applying [`Self::mask`].
    pub signature: u32,
    /// Signature bits that must match.
    pub mask: u32,
}

impl CpuIdMatch {
    /// Match all signature bits.
    pub const EXACT_MASK: u32 = 0xffff_ffff;
    /// Ignore stepping bits.
    pub const ALL_STEPPINGS_MASK: u32 = 0xffff_fff0;

    /// Return true if this entry matches `identity`.
    pub fn matches(self, identity: CpuIdentity) -> bool {
        self.vendor == identity.vendor
            && (identity.signature & self.mask) == (self.signature & self.mask)
    }
}

/// Imperative CPU model driver.
///
/// This is the Rust equivalent of coreboot's `struct cpu_driver`: generic MP
/// bringup identifies each CPU, finds a matching driver, and calls the driver
/// to perform model-specific MSR/MTRR/cache/microcode work.
pub trait CpuDriver: Send + Sync {
    /// Human-readable CPU model name.
    fn name(&self) -> &'static str;

    /// CPUID match table for this driver.
    fn id_table(&self) -> &'static [CpuIdMatch];

    /// Called on *every* logical CPU before [`Self::init_cpu`].
    ///
    /// Implement vendor-specific microcode loading here. Generic MP code does
    /// not know Intel, AMD, or any update format.
    fn update_microcode(&self) {}

    /// Called on *every* logical CPU after AP bringup.
    ///
    /// Configure model-specific MSRs here: C-state config, SpeedStep/EIST,
    /// thermal monitoring, VMX feature control, AMD PSP-related MSRs, etc.
    fn init_cpu(&self);

    /// BSP-only: called before SIPI, after LAPIC setup.
    fn pre_mp_init(&self) {}

    /// BSP-only: install the final BSP cache policy and capture the MSR image
    /// that SIPI replays on every AP before it enables caching.
    ///
    /// The default installs the architectural MTRR solution from the
    /// published RAM ranges and mirrors MTRRs/PAT. Model drivers must not
    /// recalculate MTRRs afterwards. AMD hardware drivers must override this
    /// with their RdDram/WrDram gates, SYSCFG, TOM/TOM2 and PAT policy and
    /// select `State::require_global_cache_off()` for physical shared-MSR
    /// state; the platform then owns the complete CPU set and
    /// asynchronous-event exclusion.
    fn ap_cache_state(&self) -> Result<cache::State, cache::Error> {
        // SAFETY: BSP-only, called by MP init before any AP starts.
        let msrs = unsafe { crate::x86::mtrr::snapshot_for_sipi() }
            .map_err(|_| cache::Error::Setup)?;
        cache::State::from_writes(
            msrs.iter()
                .map(|m| cache::Write::new(m.index, u64::from(m.low) | (u64::from(m.high) << 32))),
        )
    }

    /// BSP-only: called after all CPUs are initialized and APs are parked.
    fn post_mp_init(&self) {}

    /// Return true if this driver supports `identity`.
    fn matches(&self, identity: CpuIdentity) -> bool {
        self.id_table().iter().any(|id| id.matches(identity))
    }
}

/// Generic x86 CPU driver for emulated virtual-board CPUs, whose MTRRs carry
/// ordinary architectural types regardless of the emulated CPUID vendor.
/// Never use this as an AMD hardware/RdDram cache-policy fallback.
pub struct GenericX86CpuDriver;

impl CpuDriver for GenericX86CpuDriver {
    fn name(&self) -> &'static str {
        "generic-x86"
    }

    fn id_table(&self) -> &'static [CpuIdMatch] {
        &[]
    }

    fn matches(&self, _identity: CpuIdentity) -> bool {
        true
    }

    fn init_cpu(&self) {}
}

/// Read-only proof of architectural bootstrap preparation/publication ordering.
/// Only the MP engine constructs this before calling a platform launcher. It
/// does not attest target mappings, reset state, CPU set or cache semantics.
pub struct PreparedAps {
    expected: u16,
    coordinated_cache: bool,
}
impl PreparedAps {
    pub fn expected_aps(&self) -> u16 {
        self.expected
    }
    pub fn coordinated_cache(&self) -> bool {
        self.coordinated_cache
    }
    pub fn entry_address(&self) -> u64 {
        AP_BOOTSTRAP_ADDRESS
    }
}

/// BSP-only, one-shot alternative to INIT/SIPI (e.g. SMU release from reset).
/// # Safety
/// Implementations must release only the complete validated intended CPU set
/// into the already published resident bootstrap; no reset-image patching or
/// premature release from pre-MP hooks. The platform owns reset/async/cache and
/// residency contracts. No fallback/retry/rollback after partial submission.
/// Errors must report failure, not partial success; the MP engine fail-stops.
pub unsafe trait ApLauncher {
    /// Validate the selected total count (including BSP), even for zero APs.
    /// No hardware access, release, publication or reset: only cached facts and
    /// local diagnostics. Called before MP ownership or hardware setup.
    fn validate_cpu_count(&mut self, num_cpus: u16) -> Result<(), u32>;

    fn launch(&mut self, prepared: &PreparedAps) -> Result<(), u32>;
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// CPU-only MP initialization configuration. Chipset SMM installation is
/// ordinary post-MP platform code using the returned [`MpHandle`].
pub struct MpConfig<'a> {
    pub cpu_drivers: &'a [&'a dyn CpuDriver],
    /// System-wide number of online CPUs, including the BSP. The platform
    /// supplies discovery, like coreboot's `mp_ops.get_cpu_count`.
    pub num_cpus: u16,
    /// Capacity of the platform's per-CPU storage and embedded SMM image.
    pub max_cpus: u16,
}

/// Errors from MP initialization.
#[derive(Debug)]
pub enum MpError {
    /// This boot already attempted MP initialization. Never overwrite resident
    /// bootstrap/exception state or reset CPUs that could own live borrows.
    AlreadyAttempted,
    /// Selected platform rejected the count before MP hardware/publication.
    PlatformValidationFailed(u32),
    /// Platform release failed, possibly after some APs were released. The
    /// diagnostic code belongs to the platform; no INIT/SIPI fallback occurs.
    PlatformLaunchFailed(u32),
    /// A system CPU count or capacity must include at least the BSP.
    NoCpus,
    /// The requested total CPU capacity exceeds the static MP limit.
    TooManyCpus { requested: u16, supported: u16 },
    /// Hardware reports or starts more CPUs than the configured storage can hold.
    HardwareCpuCountExceedsCapacity { reported: u32, capacity: u32 },
    /// No APs responded to INIT+SIPI within the timeout.
    NoApsResponded,
    /// Fewer APs than expected checked in.
    PartialBringup { expected: u16, actual: u16 },
    /// SIPI trampoline placement failed.
    TrampolinePlacementFailed,
    /// The CPU driver could not publish a complete compatible cache policy.
    CacheBootstrap(cache::Error),
    /// The all-CPU cache-off protocol did not complete. No cache-enable gate
    /// is opened, and the BSP remains cache-off until terminal reset/halt.
    CacheRendezvousTimeout {
        expected: u16,
        ready: u32,
        restored: u32,
    },
    /// No CPU driver matched one or more CPUs.
    UnsupportedCpu,
    /// A flight step did not receive every AP. The preceding AP callback
    /// may still own borrowed driver data; this is a non-returning failure.
    FlightStepTimeout {
        step: usize,
        expected: u16,
        arrived: usize,
    },
    /// Unwinding cannot expire configuration still borrowed by APs.
    BspUnwind,
}

/// Platform-selected terminal MP failure handling.
/// # Safety
/// Implementations MUST NOT unwind or return. Borrowed CPU drivers may still
/// be executing, so their configuration, stacks and mappings must survive
/// until reset. The platform also owns safe SMI/reset handling on this path.
pub unsafe trait MpFailStop {
    fn fail_stop(&self, error: MpError) -> !;
}
struct HaltMp;
// SAFETY: this handler never returns or unwinds. It preserves the caller's
// stack/configuration and does not release any further AP barriers.
unsafe impl MpFailStop for HaltMp {
    fn fail_stop(&self, _error: MpError) -> ! {
        // Do not call formatting/console code that could itself unwind.
        loop {
            unsafe {
                core::arch::asm!("cli", "hlt", options(nomem, nostack));
            }
        }
    }
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

// Build-time CPU storage budget selected by the board/platform plan.
// The SIPI stack arena and mailboxes use the same generated capacity.
include!(concat!(env!("OUT_DIR"), "/mp_capacity.rs"));
const MAX_APS: usize = MAX_CPUS - 1;

/// Architectural default SMBASE used before SMM relocation.
pub const SMM_DEFAULT_SMBASE: u64 = 0x30000;
/// Architectural SMM entry address for the default SMBASE.
pub const SMM_DEFAULT_ENTRY: u64 = SMM_DEFAULT_SMBASE + fstart_smm::layout::SMM_ENTRY_OFFSET;
/// Temporary stack top for the default-SMRAM entry stub.
pub const SMM_DEFAULT_ENTRY_STACK_TOP: u64 = SMM_DEFAULT_SMBASE + 0x7000;
/// Architectural 64 KiB SMM "AB segment" around the default SMBASE: the entry
/// stub, its stack and the relocation identity tables are written here while
/// SMM relocates. Platforms must exclude it from OS-visible RAM on every path
/// that re-runs MP/SMM init, including an S3 resume.
pub const SMM_DEFAULT_ASEG: (u64, u64) = (SMM_DEFAULT_SMBASE, SMM_DEFAULT_SMBASE + 0x1_0000);

/// Static mailbox array.  One slot per AP (index 0 = AP #1, etc.).
/// Placed in BSS (zero-init = idle).
static MAILBOXES: [ApMailbox; MAX_APS] = {
    // const-init workaround: can't use array::from_fn in const
    const MB: ApMailbox = ApMailbox::new();
    [MB; MAX_APS]
};

const AP_STACK_SIZE: usize = 4 * 1024;

#[repr(C, align(16))]
struct ApStacks([[u8; AP_STACK_SIZE]; MAX_APS]);

static mut AP_STACKS: ApStacks = ApStacks([[0; AP_STACK_SIZE]; MAX_APS]);

/// Atomic counter: number of APs that have checked in.
static AP_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Flag: tells APs to enter the mailbox loop (vs flight plan).
static AP_IN_MAILBOX_LOOP: AtomicU8 = AtomicU8::new(0);

/// Total logical CPUs brought online by the most recent MP init.
static ONLINE_CPUS: AtomicUsize = AtomicUsize::new(0);
// Zero-initialized resident storage, not initialized writable reset-image data.
static MP_ATTEMPTED: AtomicU8 = AtomicU8::new(0);
static mut AP_EXCEPTION_TABLES: exception::Tables = exception::Tables::ZERO;

// ---------------------------------------------------------------------------
// Flight plan (internal)
// ---------------------------------------------------------------------------

/// One step of the flight plan (coreboot's `mp_flight_record`).
///
/// Every step is barriered: APs announce their arrival and wait, the BSP
/// waits for all of them, runs [`bsp`](Self::bsp), then releases the barrier
/// and the APs run [`ap`](Self::ap). Sides without work keep the defaults.
trait FlightStep: Sync {
    fn bsp(&self) {}
    fn ap(&self) {}
}

/// Every CPU identifies itself and runs the matching CPU driver.
struct CpuInit;
impl FlightStep for CpuInit {
    fn bsp(&self) {
        cpu_init();
    }
    fn ap(&self) {
        cpu_init();
    }
}

/// APs park in the mailbox loop; the BSP continues.
struct ParkAps;
impl FlightStep for ParkAps {
    fn ap(&self) {
        ap_mailbox_loop();
    }
}

const FLIGHT_PLAN: &[&dyn FlightStep] = &[&CpuInit, &ParkAps];
const MAX_FLIGHT_STEPS: usize = FLIGHT_PLAN.len();

/// Per-step rendezvous state, reset by the BSP before APs start.
struct StepSync {
    /// 0 = APs blocked, 1 = APs may proceed.
    barrier: AtomicUsize,
    /// Number of APs that have reached this step.
    cpus_entered: AtomicUsize,
}

static STEP_SYNC: [StepSync; MAX_FLIGHT_STEPS] = [const {
    StepSync {
        barrier: AtomicUsize::new(0),
        cpus_entered: AtomicUsize::new(0),
    }
}; MAX_FLIGHT_STEPS];

// ---------------------------------------------------------------------------
// Trampoline types used by the flight plan
// ---------------------------------------------------------------------------

/// The configuration of the running [`mp_init`], published for the
/// monomorphic `fn()` flight-plan callbacks that APs execute.
///
/// Set before any AP starts and cleared before `mp_init` returns, so the
/// borrowed CPU drivers outlive every access through it.
static MP_CONFIG: AtomicPtr<MpConfig<'static>> = AtomicPtr::new(core::ptr::null_mut());
static CPU_INIT_ERRORS: AtomicUsize = AtomicUsize::new(0);

/// Even a BSP callback panic must not unwind past the driver's lifetime while
/// AP work is active. Firmware uses panic=abort; this also guards unwind hosts.
struct MpBorrowGuard<'a, F: MpFailStop> {
    failure: &'a F,
    active: bool,
}
impl<F: MpFailStop> Drop for MpBorrowGuard<'_, F> {
    fn drop(&mut self) {
        if self.active {
            self.failure.fail_stop(MpError::BspUnwind);
        }
    }
}

fn publish_config(config: &MpConfig<'_>) {
    MP_CONFIG.store(
        core::ptr::from_ref(config)
            .cast_mut()
            .cast::<MpConfig<'static>>(),
        Ordering::Release,
    );
}

fn clear_mp_globals() {
    MP_CONFIG.store(core::ptr::null_mut(), Ordering::Release);
}

fn active_config() -> Option<&'static MpConfig<'static>> {
    let config = MP_CONFIG.load(Ordering::Acquire);
    // SAFETY: published by `publish_config()` from a borrow that `mp_init`
    // keeps alive until it clears the pointer; the callee never outlives it.
    (!config.is_null()).then(|| unsafe { &*config })
}

fn load_cpu_drivers() -> &'static [&'static dyn CpuDriver] {
    active_config().map_or(&[], |config| config.cpu_drivers)
}

fn find_cpu_driver(identity: CpuIdentity) -> Option<&'static dyn CpuDriver> {
    load_cpu_drivers()
        .iter()
        .copied()
        .find(|driver| driver.matches(identity))
}

fn cpu_init() {
    let identity = CpuIdentity::current();
    if let Some(driver) = find_cpu_driver(identity) {
        fstart_log::info!(
            "cpu: init cpu{} with {} (family {:#x} model {:#x})",
            current_cpu_index(),
            driver.name(),
            identity.family(),
            identity.model(),
        );
        driver.update_microcode();
        driver.init_cpu();
    } else {
        let vendor = match identity.vendor {
            CpuVendor::Intel => "Intel",
            CpuVendor::Amd => "AMD",
            CpuVendor::Other => "other",
        };
        fstart_log::error!(
            "cpu: no driver for cpu{} vendor {} signature {:#x}",
            current_cpu_index(),
            vendor,
            identity.signature,
        );
        CPU_INIT_ERRORS.fetch_add(1, Ordering::AcqRel);
    }
}

fn pre_mp_cpu_drivers(drivers: &[&dyn CpuDriver]) {
    for driver in drivers {
        driver.pre_mp_init();
    }
}

fn post_mp_cpu_drivers(drivers: &[&dyn CpuDriver]) {
    for driver in drivers {
        driver.post_mp_init();
    }
}

/// AP mailbox loop — the terminal flight plan step for APs.
fn ap_mailbox_loop() {
    AP_IN_MAILBOX_LOOP.store(1, Ordering::Release);

    // Determine our CPU index from the AP counter.
    // (Each AP atomically claimed an index during bringup.)
    // We find our mailbox by reading the cpu_index stored in each slot.
    let my_index = current_cpu_index();
    if my_index == 0 || my_index as usize > MAX_APS {
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
pub fn current_cpu_index() -> u32 {
    let lapic = Lapic::from_msr();
    let id = lapic.id();
    // Search mailboxes for our LAPIC ID.
    // During bringup, we store the LAPIC ID in cpu_index.
    // BSP always has index 0.
    if Lapic::is_bsp() {
        return 0;
    }
    for i in 0..MAX_APS {
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
#[unsafe(no_mangle)]
pub extern "C" fn fstart_ap_entry(index: u32) -> ! {
    // Assembly installed resident protected/long-mode fatal IDTs on the AP's
    // private stack before compiled code. Only now unmask virtual-wire NMI.
    // This does not establish earlier platform NMI/SMI exclusion or IST stacks.
    let lapic = Lapic::from_msr();
    lapic.enable();
    lapic.setup_virtual_wire(false);

    // Store our LAPIC ID in the mailbox so BSP can identify us.
    let ap_slot = index as usize;
    if ap_slot < MAX_APS {
        MAILBOXES[ap_slot]
            .cpu_index
            .store(lapic.id() as usize, Ordering::Release);
    }

    // Increment the global AP counter — BSP is waiting for this.
    AP_COUNT.fetch_add(1, Ordering::Release);

    // Walk the flight plan.
    for (step, sync) in FLIGHT_PLAN.iter().zip(STEP_SYNC.iter()) {
        // Signal that we've reached this step.
        sync.cpus_entered.fetch_add(1, Ordering::Release);

        // Wait for the barrier (BSP releases it after all APs check in).
        while sync.barrier.load(Ordering::Acquire) == 0 {
            core::hint::spin_loop();
        }

        step.ap();
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

/// Return the number of logical CPUs brought online by the most recent MP init.
pub fn online_cpus() -> u16 {
    ONLINE_CPUS.load(Ordering::Acquire).max(1) as u16
}

/// Publish the resident bootstrap, submit the selected launch and wait for every expected AP
/// to check in. Zero or partial check-in is boot-fatal before barriers open.
/// The trampoline resets the machine on a responder beyond `max_aps` before it
/// can index the stack arena, and this function rejects the machine if one
/// claimed an ordinal before the count is read.
fn start_aps(
    max_aps: u16,
    lapic: &Lapic,
    launcher: Option<&mut dyn ApLauncher>,
) -> Result<u16, MpError> {
    let coordinated = install_sipi_trampoline(max_aps, lapic)?;
    // No Drop implementation may restore caching on a failed/partial launch.
    // The borrowed-config guard and terminal policy retain all bootstrap state.
    let cache_off = if coordinated {
        let state = BspCacheOff::enter()?;
        publish_sipi_flag(sipi_blob::BSP_CACHE_OFF_OFFSET);
        Some(state)
    } else {
        None
    };
    let progress = || {
        if coordinated {
            read_sipi_u32(sipi_blob::CACHE_DONE_OFFSET)
        } else {
            AP_COUNT.load(Ordering::Acquire) as u32
        }
    };

    let platform_launch = submit_launch(launcher, max_aps, coordinated, || {
        fstart_log::info!("mp: sending INIT IPI");
        lapic.send_init_all_but_self();
        // Intel SDM: 10 ms after INIT, then first SIPI and 200 us.
        delay_us(10_000);
        lapic.send_sipi_all_but_self(SIPI_VECTOR_PAGE as u8);
        delay_us(200);
    })?;

    // Check if all APs responded.
    if progress() < u32::from(max_aps) {
        // Only the architectural INIT/SIPI path permits a second SIPI. Never
        // resubmit a platform release or fall back after a partial launch.
        if !platform_launch {
            lapic.send_sipi_all_but_self(SIPI_VECTOR_PAGE as u8);
        }
        // Wait up to 50 ms per AP.
        let timeout_us = 50_000u64 * max_aps as u64;
        let mut elapsed = 0u64;
        while progress() < u32::from(max_aps) && elapsed < timeout_us {
            delay_us(50);
            elapsed += 50;
        }
    }

    if let Some(cache_off) = cache_off {
        complete_cache_rendezvous(
            read_sipi_u32(sipi_blob::CACHE_READY_OFFSET),
            read_sipi_u32(sipi_blob::CACHE_DONE_OFFSET),
            max_aps,
            || cache_off.finish(),
        )?;
        // Restoration is not Rust check-in or CPU initialization completion.
        let mut remaining = 1_000 * u32::from(max_aps);
        while AP_COUNT.load(Ordering::Acquire) < usize::from(max_aps) && remaining != 0 {
            delay_us(50);
            remaining -= 1;
        }
    }
    let final_count = AP_COUNT.load(Ordering::Acquire) as u16;

    // Responders beyond `max_aps` reset the machine in the trampoline without
    // touching the stack arena. Any that already claimed an ordinal make the
    // capacity mismatch explicit before that reset lands.
    let claimed = sipi_claimed_ap_count();
    if claimed > u32::from(max_aps) {
        return Err(MpError::HardwareCpuCountExceedsCapacity {
            reported: claimed.saturating_add(1),
            capacity: u32::from(max_aps) + 1,
        });
    }

    complete_ap_bringup(final_count, max_aps)
}

fn submit_launch(
    launcher: Option<&mut dyn ApLauncher>,
    expected: u16,
    coordinated_cache: bool,
    init_sipi: impl FnOnce(),
) -> Result<bool, MpError> {
    if let Some(launcher) = launcher {
        launcher
            .launch(&PreparedAps {
                expected,
                coordinated_cache,
            })
            .map_err(MpError::PlatformLaunchFailed)?;
        Ok(true)
    } else {
        init_sipi();
        Ok(false)
    }
}

fn complete_ap_bringup(final_count: u16, max_aps: u16) -> Result<u16, MpError> {
    // Do not release any flight-plan barrier for a partial set. A responder
    // that claimed a valid stack ordinal but has not reached `AP_COUNT` yet
    // could otherwise arrive after the BSP snapshots `final_count`, pass
    // barriers released for the smaller set, and outlive the borrowed MP
    // configuration. Returning here leaves every late AP behind a closed
    // barrier until the boot-fatal error path takes over.
    if final_count == 0 {
        return Err(MpError::NoApsResponded);
    }
    if final_count < max_aps {
        return Err(MpError::PartialBringup {
            expected: max_aps,
            actual: final_count,
        });
    }

    fstart_log::info!("mp: {}/{} APs checked in", final_count, max_aps);
    Ok(final_count)
}

/// Initialize all CPUs, run CPU-model initialization, and leave APs in the
/// scoped-work mailbox loop. Chipset SMM setup is deliberately post-MP code.
pub fn mp_init(config: &MpConfig<'_>) -> Result<MpHandle, MpError> {
    mp_init_with_fail_stop(config, &HaltMp)
}

/// As `mp_init`, with a platform reset/fail-stop policy. A failed launch or
/// flight rendezvous cannot return and expire driver borrows still used by
/// APs. Pre-launch validation and fully completed CPU errors may return.
pub fn mp_init_with_fail_stop(
    config: &MpConfig<'_>,
    failure: &impl MpFailStop,
) -> Result<MpHandle, MpError> {
    mp_init_selected(config, failure, None)
}

/// As `mp_init_with_fail_stop`, but launch through a platform protocol only
/// after generic preparation/publication (and selected BSP cache-off entry).
/// Selected count validation includes BSP-only configurations and precedes all
/// MP hardware setup. A failed/partial release invokes terminal handling, never
/// another launcher.
pub fn mp_init_with_ap_launcher(
    config: &MpConfig<'_>,
    failure: &impl MpFailStop,
    launcher: &mut impl ApLauncher,
) -> Result<MpHandle, MpError> {
    mp_init_selected(config, failure, Some(launcher))
}

fn mp_init_selected(
    config: &MpConfig<'_>,
    failure: &impl MpFailStop,
    mut launcher: Option<&mut dyn ApLauncher>,
) -> Result<MpHandle, MpError> {
    let num_cpus = validate_mp_configuration(config, &mut launcher)?;
    let max_aps = num_cpus.saturating_sub(1);
    claim_mp_attempt(&MP_ATTEMPTED)?;

    fstart_log::info!(
        "mp: initializing {} CPUs (max_cpus bound {})",
        num_cpus,
        config.max_cpus
    );

    // --- Step 1: BSP LAPIC setup ---
    let lapic = Lapic::from_msr();
    lapic.enable();
    lapic.setup_virtual_wire(true);

    fstart_log::info!("mp: BSP LAPIC ID = {}", lapic.id());

    // Pre-MP CPU-driver hooks (BSP only).
    pre_mp_cpu_drivers(config.cpu_drivers);

    // APs reach the drivers through the published config; it is
    // withdrawn before returning on every path.
    let mut borrow_guard = MpBorrowGuard {
        failure,
        active: true,
    };
    publish_config(config);
    let flown = fly(max_aps, &lapic, failure, launcher);
    // Only a completed flight can get here: no AP retains a driver callback.
    clear_mp_globals();
    borrow_guard.active = false;
    let final_count = flown?;

    post_mp_cpu_drivers(config.cpu_drivers);
    ONLINE_CPUS.store((final_count + 1) as usize, Ordering::Release);
    fstart_log::info!("mp: initialization complete ({} CPUs)", final_count + 1);

    Ok(MpHandle {
        num_aps: final_count,
        _not_send: PhantomData,
    })
}

/// Pure preflight, independent of whether this configuration needs AP release.
fn validate_mp_configuration(
    config: &MpConfig<'_>,
    launcher: &mut Option<&mut dyn ApLauncher>,
) -> Result<u16, MpError> {
    validate_cpu_capacity(config.max_cpus)?;
    let count = checked_logical_cpu_count(config.num_cpus, config.max_cpus)?;
    if let Some(selected) = launcher {
        selected
            .validate_cpu_count(count)
            .map_err(MpError::PlatformValidationFailed)?;
    }
    Ok(count)
}

/// Start the APs and walk the CPU-only flight plan on the BSP side.
fn fly(
    max_aps: u16,
    lapic: &Lapic,
    failure: &impl MpFailStop,
    launcher: Option<&mut dyn ApLauncher>,
) -> Result<u16, MpError> {
    CPU_INIT_ERRORS.store(0, Ordering::Release);
    AP_COUNT.store(0, Ordering::Release);
    AP_IN_MAILBOX_LOOP.store(0, Ordering::Release);

    for sync in STEP_SYNC.iter() {
        sync.barrier.store(0, Ordering::Release);
        sync.cpus_entered.store(0, Ordering::Release);
    }
    let plan = FLIGHT_PLAN;
    fence(Ordering::SeqCst);

    let final_count = if max_aps == 0 {
        0
    } else {
        match start_aps(max_aps, lapic, launcher) {
            Ok(count) => count,
            Err(error) => failure.fail_stop(error),
        }
    };

    // Walk the flight plan on the BSP side; APs walk it in `fstart_ap_entry`.
    for (i, (step, sync)) in plan.iter().zip(STEP_SYNC.iter()).enumerate() {
        if let Err(error) = rendezvous_step(*step, sync, i, final_count, delay_us) {
            // Keep this and all later barriers closed. In particular, an AP
            // can still be inside the preceding init_cpu callback: neither
            // clear MP_CONFIG nor return/drop the borrowed driver.
            failure.fail_stop(error);
        }
    }

    if CPU_INIT_ERRORS.load(Ordering::Acquire) != 0 {
        return Err(MpError::UnsupportedCpu);
    }
    Ok(final_count)
}

/// Arrival at step N proves completion of each AP's step N-1 callback.
/// Timeout is not that proof: no BSP work or barrier release may follow it.
fn rendezvous_step(
    step: &dyn FlightStep,
    sync: &StepSync,
    index: usize,
    expected: u16,
    mut delay: impl FnMut(u64),
) -> Result<(), MpError> {
    let mut remaining = 10_000;
    loop {
        let arrived = sync.cpus_entered.load(Ordering::Acquire);
        if arrived >= usize::from(expected) {
            break;
        }
        if remaining == 0 {
            return Err(MpError::FlightStepTimeout {
                step: index,
                expected,
                arrived,
            });
        }
        delay(100);
        remaining -= 1;
    }
    step.bsp();
    sync.barrier.store(1, Ordering::Release);
    Ok(())
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
/// Resident entry address for platform reset dispatch as well as INIT/SIPI.
/// This is an ABI address, not evidence that publication/reservation is ready.
pub const AP_BOOTSTRAP_ADDRESS: u64 = SIPI_VECTOR_ADDR as u64;

/// Install the SIPI trampoline at the vector address.
///
/// Copies the trampoline code to `SIPI_VECTOR_ADDR` and patches the
/// parameter block (GDT, stack, CR3, AP entry point, etc.).
fn sipi_claimed_ap_count() -> u32 {
    // SAFETY: the trampoline page remains reserved and mapped throughout MP
    // bring-up. APs update this aligned field atomically before either taking a
    // stack slot or resetting as excess hardware.
    unsafe {
        core::ptr::read_volatile(
            (SIPI_VECTOR_ADDR as *const u8).add(sipi_blob::AP_COUNTER_OFFSET) as *const u32,
        )
    }
}

#[cfg(test)]
mod sipi_tests;

/// Copy packed 12-byte MSR records (index, low, high) into the SIPI page.
fn patch_msr_table(page: &mut [u8], records: &[u8]) -> Result<(), MpError> {
    let end = sipi_blob::MSR_TABLE_OFFSET
        .checked_add(records.len())
        .ok_or(MpError::TrampolinePlacementFailed)?;
    let count_end = sipi_blob::MSR_COUNT_OFFSET + core::mem::size_of::<u32>();
    if records.len() % 12 != 0 || end > page.len() || count_end > page.len() {
        return Err(MpError::TrampolinePlacementFailed);
    }
    page[sipi_blob::MSR_TABLE_OFFSET..end].copy_from_slice(records);
    page[sipi_blob::MSR_COUNT_OFFSET..count_end]
        .copy_from_slice(&((records.len() / 12) as u32).to_le_bytes());
    Ok(())
}

fn validate_bootstrap_addresses(cr3: u64, stack_base: u64, max_aps: u16) -> Result<(), MpError> {
    // Protected-mode bootstrap uses 32-bit CR3/ESP. Reject truncation and
    // arithmetic wrap, not only a nominally below-4-GiB arena base. The
    // 32-bit target runs without paging, so CR3 is only meaningful for
    // long mode.
    let end = stack_base.checked_add(u64::from(max_aps) * AP_STACK_SIZE as u64);
    if (cfg!(target_arch = "x86_64") && cr3 == 0)
        || cr3 > u64::from(u32::MAX)
        || cr3 & 0xfff != 0
        || stack_base == 0
        || stack_base & 15 != 0
        || end.is_none_or(|end| end > u64::from(u32::MAX))
    {
        return Err(MpError::TrampolinePlacementFailed);
    }
    Ok(())
}

fn install_sipi_trampoline(max_aps: u16, _lapic: &Lapic) -> Result<bool, MpError> {
    if max_aps as usize > MAX_APS || sipi_blob::TRAMPOLINE.len() > 4096 {
        return Err(MpError::TrampolinePlacementFailed);
    }

    let driver = find_cpu_driver(CpuIdentity::current()).ok_or(MpError::UnsupportedCpu)?;
    let cache = driver.ap_cache_state().map_err(MpError::CacheBootstrap)?;
    let dst = SIPI_VECTOR_ADDR as *mut u8;
    // SAFETY: we only expose the raw stack arena address to AP startup code;
    // Rust never creates references to individual AP stacks while they run.
    let stack_base = unsafe { core::ptr::addr_of_mut!(AP_STACKS.0) as u64 };
    let cr3 = read_cr3();
    validate_bootstrap_addresses(cr3, stack_base, max_aps)?;
    // SAFETY: single-attempt ownership excludes live readers; tables remain
    // resident until reset. Physical identity mapping/reservation are platform
    // prerequisites, not consequences of the checked numerical geometry.
    let (idtr32, idtr64) = unsafe {
        exception::prepare(
            core::ptr::addr_of_mut!(AP_EXCEPTION_TABLES),
            (SIPI_VECTOR_ADDR + sipi_blob::FAULT32_OFFSET) as u64,
            (SIPI_VECTOR_ADDR + sipi_blob::FAULT64_OFFSET) as u64,
        )
    }
    .ok_or(MpError::TrampolinePlacementFailed)?;
    // SAFETY: SIPI_VECTOR_ADDR is a conventional-memory page reserved for AP
    // startup. The copied blob occupies one page and all patch offsets are
    // emitted by the build script from symbols inside that blob.
    unsafe {
        core::ptr::copy_nonoverlapping(
            sipi_blob::TRAMPOLINE.as_ptr(),
            dst,
            sipi_blob::TRAMPOLINE.len(),
        );

        patch_u64(dst, sipi_blob::CR3_OFFSET, cr3);
        patch_u64(
            dst,
            sipi_blob::ENTRY_OFFSET,
            fstart_ap_entry as *const () as usize as u64,
        );
        patch_u64(dst, sipi_blob::STACK_BASE_OFFSET, stack_base);
        patch_u32(dst, sipi_blob::STACK_SIZE_OFFSET, AP_STACK_SIZE as u32);
        patch_u32(dst, sipi_blob::AP_LIMIT_OFFSET, u32::from(max_aps));
        patch_u32(dst, sipi_blob::AP_COUNTER_OFFSET, 0);
        core::ptr::copy_nonoverlapping(
            idtr32.as_bytes().as_ptr(),
            dst.add(sipi_blob::IDTR32_OFFSET),
            10,
        );
        core::ptr::copy_nonoverlapping(
            idtr64.as_bytes().as_ptr(),
            dst.add(sipi_blob::IDTR64_OFFSET),
            10,
        );
        patch_u32(
            dst,
            sipi_blob::CACHE_COORDINATED_OFFSET,
            u32::from(cache.global_cache_off()),
        );
        patch_msr_table(
            core::slice::from_raw_parts_mut(dst, sipi_blob::TRAMPOLINE.len()),
            cache.bytes(),
        )?;

        // INIT leaves AP caches disabled. Write the complete copied and patched
        // trampoline back to DRAM before sending SIPI, as coreboot does for its
        // SIPI module.
        crate::x86::writeback_cache_range(dst, sipi_blob::TRAMPOLINE.len());
    }

    // Diagnostic: the AP's first fetch depends entirely on this page and the
    // values patched into it. A wrong entry or stack turns into a triple fault
    // on the AP, which the chipset reports as a platform reset, so log both the
    // copied bytes and the patch values.
    // SAFETY: `dst` points at the copied page, which holds at least one u32.
    let words = unsafe { core::ptr::read_unaligned(dst as *const [u32; 4]) };
    // SAFETY: the entry symbol is a function in this stage.
    let entry = fstart_ap_entry as *const () as usize as u64;
    fstart_log::info!("mp: tramp[0]={:#010x} [4]={:#010x}", words[0], words[1]);
    fstart_log::info!("mp: tramp[8]={:#010x} [12]={:#010x}", words[2], words[3]);
    fstart_log::info!(
        "mp: mirroring {} BSP cache-policy MSRs before AP cache enable{}",
        cache.writes().len(),
        if cache.global_cache_off() { " (coordinated)" } else { "" }
    );
    fstart_log::info!("mp: patch cr3={:#x}", read_cr3());
    fstart_log::info!("mp: patch entry={:#x} base={:#x}", entry, stack_base);

    fstart_log::info!(
        "mp: SIPI trampoline at {:#x}, AP stacks at {:#x}, {} bytes each",
        SIPI_VECTOR_ADDR,
        stack_base as usize,
        AP_STACK_SIZE
    );
    Ok(cache.global_cache_off())
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

// All aligned fields here live in the copied resident trampoline, not the
// immutable reset image. No Rust reference aliases the assembly atomic writers.
fn read_sipi_u32(offset: usize) -> u32 {
    let value = unsafe { core::ptr::read_volatile((SIPI_VECTOR_ADDR + offset) as *const u32) };
    fence(Ordering::Acquire);
    value
}

fn publish_sipi_flag(offset: usize) {
    fence(Ordering::SeqCst);
    unsafe { core::ptr::write_volatile((SIPI_VECTOR_ADDR + offset) as *mut u32, 1) };
    fence(Ordering::SeqCst);
}

fn complete_cache_rendezvous(
    ready: u32,
    restored: u32,
    expected: u16,
    finish: impl FnOnce(),
) -> Result<(), MpError> {
    if ready != u32::from(expected) || restored != u32::from(expected) {
        return Err(MpError::CacheRendezvousTimeout {
            expected,
            ready,
            restored,
        });
    }
    finish();
    Ok(())
}

/// Explicit BSP participation, selected only by a coordinated image. Ordinary
/// Intel/virtual paths retain their existing independent protocol: some Intel
/// platforms cannot run WBINVD with CD=1 (see x86::disable_cache).
/// No automatic cleanup/re-enable on failure or unwind.
struct BspCacheOff {
    cr0: usize,
    cr4: usize,
}
impl BspCacheOff {
    fn enter() -> Result<Self, MpError> {
        let (cr0, cr4, flags): (usize, usize, usize);
        unsafe {
            core::arch::asm!(
                "mov {cr0}, cr0", "mov {cr4}, cr4", "pushf", "pop {flags}",
                cr0 = out(reg) cr0, cr4 = out(reg) cr4, flags = out(reg) flags,
                options(preserves_flags),
            );
        }
        // The caller owns NMI/SMI exclusion; CLI alone cannot establish it.
        // PCID is not supported by this firmware bootstrap/TLB protocol.
        if flags & (1 << 9) != 0 || cr4 & (1 << 17) != 0 || cr0 & ((1 << 29) | (1 << 30)) != 0 {
            return Err(MpError::CacheBootstrap(cache::Error::Unsupported));
        }
        let disabled_cr0 = (cr0 | (1 << 30)) & !(1 << 29);
        let disabled_cr4 = cr4 & !(1 << 7);
        unsafe {
            core::arch::asm!(
                "mov cr0, {cr0}", "wbinvd", "mov cr4, {cr4}",
                "mov {scratch}, cr3", "mov cr3, {scratch}",
                cr0 = in(reg) disabled_cr0, cr4 = in(reg) disabled_cr4,
                scratch = out(reg) _, options(nostack, preserves_flags),
            );
        }
        Ok(Self { cr0, cr4 })
    }

    fn finish(self) {
        // Every AP has restored the same final image and remains cache-off.
        // PGE is still clear; invalidate cache/TLB state before opening the gate.
        unsafe {
            core::arch::asm!(
                "wbinvd", "mov {scratch}, cr3", "mov cr3, {scratch}",
                scratch = out(reg) _, options(nostack, preserves_flags),
            );
        }
        // Publish while BSP writes are uncached, before any CPU enables cache.
        publish_sipi_flag(sipi_blob::CACHE_RELEASE_OFFSET);
        unsafe {
            core::arch::asm!(
                "mov cr0, {cr0}", "mov cr4, {cr4}",
                cr0 = in(reg) self.cr0, cr4 = in(reg) self.cr4,
                options(nostack, preserves_flags),
            );
        }
    }
}

fn read_cr3() -> u64 {
    // SAFETY: reading CR3 is side-effect-free and needed to let APs enter the
    // same identity-mapped long-mode address space as the BSP.
    unsafe { crate::x86::controlregs::cr3() }
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
    _not_send: PhantomData<*mut ()>,
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
        if wait_for_aps_parked(self.num_aps as usize) {
            fstart_log::info!("mp: {} APs parked", self.num_aps);
        } else {
            fstart_log::error!("mp: timed out parking {} APs", self.num_aps);
        }
    }
}

/// Park any APs brought up by [`mp_init`] before handing control to a payload or OS.
///
/// `mp_init` leaves APs in the mailbox loop so firmware can run scoped work on
/// them.  An OS does not know about that mailbox and expects APs to be quiescent
/// until it sends its own INIT/SIPI sequence.  Call this immediately before a
/// final payload jump.
#[must_use]
pub fn park_aps_for_payload() -> bool {
    let num_aps = AP_COUNT.load(Ordering::Acquire).min(MAX_APS);
    if num_aps == 0 {
        return true;
    }

    for i in 0..num_aps {
        let mb = &MAILBOXES[i];
        mb.data.store(0, Ordering::Relaxed);
        mb.func
            .store(park_cpu as *const () as usize, Ordering::Release);
    }

    if wait_for_aps_parked(num_aps) {
        fstart_log::info!("mp: {} APs parked for payload handoff", num_aps as u32);
        true
    } else {
        fstart_log::error!(
            "mp: timed out parking {} APs for payload handoff",
            num_aps as u32
        );
        false
    }
}

/// HLT loop for parking an AP.
fn park_cpu(_data: *const (), cpu: u32) {
    // Acknowledge the command before stopping. The trampoline never returns,
    // so the mailbox loop cannot perform its normal completion store.
    if let Some(mb) = cpu
        .checked_sub(1)
        .and_then(|index| MAILBOXES.get(index as usize))
    {
        mb.func.store(MB_IDLE, Ordering::Release);
    }

    // Keep the AP quiescent until the payload's INIT/SIPI sequence. Maskable
    // firmware interrupts must not wake it back into firmware-owned state.
    // SAFETY: disabling interrupts and halting is the terminal AP handoff path.
    unsafe { core::arch::asm!("cli", options(nomem, nostack)) };
    loop {
        // SAFETY: HLT is always safe.
        unsafe { core::arch::asm!("hlt", options(nomem, nostack)) };
    }
}

fn wait_for_aps_parked(num_aps: usize) -> bool {
    const TIMEOUT_US: usize = 100_000;

    for _ in 0..TIMEOUT_US {
        if MAILBOXES[..num_aps]
            .iter()
            .all(|mb| mb.func.load(Ordering::Acquire) == MB_IDLE)
        {
            return true;
        }
        delay_us(1);
    }
    false
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

fn claim_mp_attempt(attempted: &AtomicU8) -> Result<(), MpError> {
    attempted
        .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
        .map(|_| ())
        .map_err(|_| MpError::AlreadyAttempted)
}

fn validate_cpu_capacity(max_cpus: u16) -> Result<(), MpError> {
    if max_cpus == 0 {
        Err(MpError::NoCpus)
    } else if usize::from(max_cpus) > MAX_CPUS {
        Err(MpError::TooManyCpus {
            requested: max_cpus,
            supported: MAX_CPUS as u16,
        })
    } else {
        Ok(())
    }
}

fn checked_logical_cpu_count(count: u16, max_cpus: u16) -> Result<u16, MpError> {
    if count == 0 {
        Err(MpError::NoCpus)
    } else if count > max_cpus {
        Err(MpError::HardwareCpuCountExceedsCapacity {
            reported: u32::from(count),
            capacity: u32::from(max_cpus),
        })
    } else {
        Ok(count)
    }
}

fn delay_us(us: u64) {
    crate::x86::udelay(us.min(u32::MAX as u64) as u32);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_launch_is_one_shot_and_never_falls_back_to_init_sipi() {
        struct Launcher {
            calls: usize,
            result: Result<(), u32>,
        }
        // SAFETY: pure test adapter releases no CPUs or hardware resources.
        unsafe impl ApLauncher for Launcher {
            fn validate_cpu_count(&mut self, count: u16) -> Result<(), u32> {
                assert_eq!(count, 8);
                Ok(())
            }
            fn launch(&mut self, prepared: &PreparedAps) -> Result<(), u32> {
                assert_eq!(prepared.expected_aps(), 7);
                assert!(prepared.coordinated_cache());
                assert_eq!(prepared.entry_address(), 0x8000);
                self.calls += 1;
                self.result
            }
        }
        for result in [Ok(()), Err(0x1234)] {
            let mut launcher = Launcher { calls: 0, result };
            let actual = submit_launch(Some(&mut launcher), 7, true, || {
                panic!("unexpected INIT/SIPI")
            });
            assert_eq!(launcher.calls, 1);
            if result.is_ok() {
                assert!(matches!(actual, Ok(true)));
            } else {
                assert!(matches!(actual, Err(MpError::PlatformLaunchFailed(0x1234))));
            }
        }
        let mut sent = false;
        assert!(matches!(
            submit_launch(None, 7, false, || sent = true),
            Ok(false)
        ));
        assert!(sent);
    }

    #[test]
    fn selected_platform_count_is_validated_even_without_aps() {
        struct Launcher {
            total: u16,
            validations: usize,
        }
        // SAFETY: cached-only validation; test submission is never invoked.
        unsafe impl ApLauncher for Launcher {
            fn validate_cpu_count(&mut self, count: u16) -> Result<(), u32> {
                self.validations += 1;
                if count == self.total {
                    Ok(())
                } else {
                    Err(0x90)
                }
            }
            fn launch(&mut self, _: &PreparedAps) -> Result<(), u32> {
                panic!("BSP-only count check must not submit APs")
            }
        }
        let config = MpConfig {
            cpu_drivers: &[],
            num_cpus: 1,
            max_cpus: 1,
        };
        let mut wrong = Launcher {
            total: 8,
            validations: 0,
        };
        // Exercise the real selected entry: rejection must precede privileged
        // LAPIC/driver/publication work and the global attempt claim.
        assert!(matches!(
            mp_init_with_ap_launcher(&config, &HaltMp, &mut wrong),
            Err(MpError::PlatformValidationFailed(0x90))
        ));
        assert_eq!(wrong.validations, 1);
        let mut singleton = Launcher {
            total: 1,
            validations: 0,
        };
        assert_eq!(
            validate_mp_configuration(&config, &mut Some(&mut singleton)).unwrap(),
            1
        );
        assert_eq!(singleton.validations, 1);
    }

    #[test]
    fn bootstrap_and_exception_storage_cannot_be_reused_this_boot() {
        let attempted = AtomicU8::new(0);
        assert!(claim_mp_attempt(&attempted).is_ok());
        assert!(matches!(
            claim_mp_attempt(&attempted),
            Err(MpError::AlreadyAttempted)
        ));
        assert_eq!(attempted.load(Ordering::Acquire), 1);
    }

    #[test]
    fn global_cache_gate_requires_all_ready_and_all_restored() {
        for (ready, restored) in [(0, 0), (6, 0), (7, 6), (6, 7), (8, 7), (7, 8)] {
            let mut enabled = false;
            assert!(matches!(
                complete_cache_rendezvous(ready, restored, 7, || enabled = true),
                Err(MpError::CacheRendezvousTimeout { .. })
            ));
            assert!(!enabled);
        }
        let mut enabled = false;
        complete_cache_rendezvous(7, 7, 7, || enabled = true).unwrap();
        assert!(enabled);
    }

    #[test]
    fn bootstrap_rejects_32bit_truncation_and_arena_wrap() {
        assert!(validate_bootstrap_addresses(0x20000, 0x100000, 7).is_ok());
        for (cr3, base) in [
            (0, 0x100000),
            (0x1_0002_0000, 0x100000),
            (0x20008, 0x100000),
            (0x20000, 0x1_0010_0000),
            (0x20000, 0xfffff000),
            (0x20000, u64::MAX - 15),
            (0x20000, 0x100001),
        ] {
            assert!(validate_bootstrap_addresses(cr3, base, 7).is_err());
        }
    }

    #[test]
    fn flight_timeout_keeps_barrier_closed_and_skips_bsp_work() {
        struct Step(AtomicUsize);
        impl FlightStep for Step {
            fn bsp(&self) {
                self.0.fetch_add(1, Ordering::Relaxed);
            }
        }
        let step = Step(AtomicUsize::new(0));
        // One AP has not finished its preceding CPU driver callback yet.
        let sync = StepSync {
            barrier: AtomicUsize::new(0),
            cpus_entered: AtomicUsize::new(1),
        };
        let mut delays = 0;
        assert!(matches!(
            rendezvous_step(&step, &sync, 1, 2, |us| {
                assert_eq!(us, 100);
                delays += 1;
            }),
            Err(MpError::FlightStepTimeout {
                step: 1,
                expected: 2,
                arrived: 1
            })
        ));
        assert_eq!(delays, 10_000);
        assert_eq!(sync.barrier.load(Ordering::Acquire), 0);
        assert_eq!(step.0.load(Ordering::Relaxed), 0);
        // A genuinely completed, delayed AP arrival permits the step.
        let mut delays = 0;
        rendezvous_step(&step, &sync, 1, 2, |_| {
            delays += 1;
            if delays == 3 {
                sync.cpus_entered.store(2, Ordering::Release);
            }
        })
        .unwrap();
        assert_eq!(delays, 3);
        assert_eq!(sync.barrier.load(Ordering::Acquire), 1);
        assert_eq!(step.0.load(Ordering::Relaxed), 1);
    }
    #[test]
    fn total_cpu_capacity_includes_the_bsp() {
        assert!(validate_cpu_capacity(MAX_CPUS as u16).is_ok());
        if let Some(over_capacity) = (MAX_CPUS as u16).checked_add(1) {
            assert!(matches!(
                validate_cpu_capacity(over_capacity),
                Err(MpError::TooManyCpus { requested, supported })
                    if requested == over_capacity && supported == MAX_CPUS as u16
            ));
        }
        assert_eq!(MAX_APS + 1, MAX_CPUS);

        assert!(matches!(validate_cpu_capacity(0), Err(MpError::NoCpus)));
        assert!(matches!(
            checked_logical_cpu_count(0, 4),
            Err(MpError::NoCpus)
        ));
        assert_eq!(checked_logical_cpu_count(4, 4).unwrap(), 4);
        assert!(matches!(
            checked_logical_cpu_count(5, 4),
            Err(MpError::HardwareCpuCountExceedsCapacity {
                reported: 5,
                capacity: 4
            })
        ));

        assert!(matches!(
            complete_ap_bringup(0, 3),
            Err(MpError::NoApsResponded)
        ));
        assert!(matches!(
            complete_ap_bringup(2, 3),
            Err(MpError::PartialBringup {
                expected: 3,
                actual: 2
            })
        ));
        assert_eq!(complete_ap_bringup(3, 3).unwrap(), 3);
    }
}
