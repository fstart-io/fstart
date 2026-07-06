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

#[allow(
    clippy::declare_interior_mutable_const,
    clippy::doc_lazy_continuation,
    clippy::missing_transmute_annotations,
    clippy::needless_range_loop
)]
use core::marker::PhantomData;
use core::sync::atomic::{fence, AtomicBool, AtomicU8, AtomicUsize, Ordering};

use crate::lapic::Lapic;

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

    /// BSP-only: called after all CPUs are initialized and APs are parked.
    fn post_mp_init(&self) {}

    /// Return true if this driver supports `identity`.
    fn matches(&self, identity: CpuIdentity) -> bool {
        self.id_table().iter().any(|id| id.matches(identity))
    }
}

/// Generic x86 CPU driver used by virtual boards until model-specific MSR
/// programming is needed.
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

    fn init_cpu(&self) {
        // SAFETY: MP init runs this on every active CPU after memory detection
        // has published the WB RAM ranges.
        unsafe { crate::x86::mtrr::setup_ram_wb() };
        fstart_log::info!("cpu: generic x86 MTRR setup complete");
    }
}

/// SMM (System Management Mode) setup operations.
///
/// Provided by the chipset/northbridge driver.  Controls TSEG geometry,
/// SMM handler installation, and per-CPU SMBASE relocation.
///
/// Designed as a separate trait from [`CpuDriver`] because SMM is a chipset
/// concern (NB owns TSEG/SMRAM, SB controls SMI routing), while `CpuDriver`
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
/// Supplies CPU model drivers plus optional [`SmmOps`].  When `smm` is
/// `None`, the SMM flight plan steps are skipped entirely.
pub struct MpConfig<'a> {
    /// CPU model drivers available to the stage.
    pub cpu_drivers: &'a [&'a dyn CpuDriver],
    /// Chipset SMM operations.  `None` = no SMM.
    pub smm: Option<&'a dyn SmmOps>,
    /// Standalone native PIC SMM image to install when `smm` is `Some`.
    pub smm_image: Option<&'a [u8]>,
    /// Maximum logical CPU count to attempt (BSP + APs).
    pub max_cpus: u16,
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
    /// No CPU driver matched one or more CPUs.
    UnsupportedCpu,
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

/// Architectural default SMBASE used before SMM relocation.
pub const SMM_DEFAULT_SMBASE: u64 = 0x30000;
/// Architectural SMM entry address for the default SMBASE.
pub const SMM_DEFAULT_ENTRY: u64 = SMM_DEFAULT_SMBASE + fstart_smm::layout::SMM_ENTRY_OFFSET;
/// Temporary stack top for the default-SMRAM entry stub.
pub const SMM_DEFAULT_ENTRY_STACK_TOP: u64 = SMM_DEFAULT_SMBASE + 0x7000;

const SMM_SAVE_STATE_SIZE: u64 = 0x1_0000;
const SMM_REVISION_OFFSET_FROM_TOP: u64 = 0x104;
const SMM_EM64T101_REVISION: u32 = 0x0003_0101;
const SMM_LEGACY_REVISION_OFFSET: u64 = 0xff04;

/// Static mailbox array.  One slot per AP (index 0 = AP #1, etc.).
/// Placed in BSS (zero-init = idle).
static MAILBOXES: [ApMailbox; MAX_CPUS] = {
    // const-init workaround: can't use array::from_fn in const
    const MB: ApMailbox = ApMailbox::new();
    [MB; MAX_CPUS]
};

const AP_STACK_SIZE: usize = 4 * 1024;

#[repr(C, align(16))]
struct ApStacks([[u8; AP_STACK_SIZE]; MAX_CPUS]);

static mut AP_STACKS: ApStacks = ApStacks([[0; AP_STACK_SIZE]; MAX_CPUS]);

/// Atomic counter: number of APs that have checked in.
static AP_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Flag: tells APs to enter the mailbox loop (vs flight plan).
static AP_IN_MAILBOX_LOOP: AtomicU8 = AtomicU8::new(0);

/// Total logical CPUs brought online by the most recent MP init.
static ONLINE_CPUS: AtomicUsize = AtomicUsize::new(1);

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

/// Global CPU-driver slice pointer — set by BSP before APs start.
static CPU_DRIVERS_PTR: AtomicUsize = AtomicUsize::new(0);
/// Global CPU-driver slice length — set by BSP before APs start.
static CPU_DRIVERS_LEN: AtomicUsize = AtomicUsize::new(0);
/// Global cpu_init trampoline.
static CPU_INIT_FN: AtomicUsize = AtomicUsize::new(0);
static CPU_INIT_ERRORS: AtomicUsize = AtomicUsize::new(0);
/// Global `&dyn SmmOps` fat pointer split into data/vtable words for
/// monomorphized `fn()` flight-plan callbacks.
static SMM_OPS_DATA: AtomicUsize = AtomicUsize::new(0);
static SMM_OPS_VTABLE: AtomicUsize = AtomicUsize::new(0);
static SMM_RELOCATION_LOCK: AtomicBool = AtomicBool::new(false);
static SMM_RELOCATION_SMBASES: [AtomicUsize; MAX_CPUS] = [const { AtomicUsize::new(0) }; MAX_CPUS];
/// Global smm_relocate trampoline.
static SMM_RELOCATE_FN: AtomicUsize = AtomicUsize::new(0);

fn store_cpu_drivers(drivers: &[&dyn CpuDriver]) {
    CPU_DRIVERS_PTR.store(drivers.as_ptr() as usize, Ordering::Release);
    CPU_DRIVERS_LEN.store(drivers.len(), Ordering::Release);
}

fn clear_cpu_drivers() {
    CPU_DRIVERS_PTR.store(0, Ordering::Release);
    CPU_DRIVERS_LEN.store(0, Ordering::Release);
}

fn clear_mp_globals() {
    clear_cpu_drivers();
    clear_smm_ops();
}

fn load_cpu_drivers() -> &'static [&'static dyn CpuDriver] {
    let ptr = CPU_DRIVERS_PTR.load(Ordering::Acquire) as *const &'static dyn CpuDriver;
    let len = CPU_DRIVERS_LEN.load(Ordering::Acquire);
    if ptr.is_null() || len == 0 {
        return &[];
    }
    // SAFETY: set by `store_cpu_drivers()` before APs start. The referenced
    // slice and drivers remain alive until `mp_init()` finishes and clears the
    // globals after all APs have entered the mailbox loop.
    unsafe { core::slice::from_raw_parts(ptr, len) }
}

fn find_cpu_driver(identity: CpuIdentity) -> Option<&'static dyn CpuDriver> {
    load_cpu_drivers()
        .iter()
        .copied()
        .find(|driver| driver.matches(identity))
}

fn cpu_init_trampoline() {
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
    while SMM_RELOCATION_LOCK
        .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        core::hint::spin_loop();
    }

    if let Some(ops) = load_smm_ops() {
        ops.smm_relocate();
    }

    SMM_RELOCATION_LOCK.store(false, Ordering::Release);
}

fn smm_post_init_trampoline() {
    if let Some(ops) = load_smm_ops() {
        ops.post_smm_init();
    }
}

/// Prepare the SMBASE lookup table used by [`default_smm_relocation_handler`].
pub fn prepare_default_smm_relocation(cpus: &[fstart_smm::CpuSmmLayout]) {
    let fallback = cpus.first().map(|cpu| cpu.smbase as usize).unwrap_or(0);
    for entry in SMM_RELOCATION_SMBASES.iter() {
        entry.store(fallback, Ordering::Release);
    }
    for (slot, cpu) in SMM_RELOCATION_SMBASES.iter().zip(cpus.iter()) {
        slot.store(cpu.smbase as usize, Ordering::Release);
    }
}

/// Default-SMRAM relocation callback used by the temporary entry stub.
///
/// The temporary stub at `0x30000 + 0x8000` enters long mode and calls this
/// function, mirroring coreboot's gen1 relocation flow where the SMM entry is
/// only a trampoline back to normal firmware code. The MP flight plan
/// serializes calls so the shared default save-state area is not corrupted.
pub extern "C" fn default_smm_relocation_handler(_params: *mut fstart_smm::SmmEntryParams) {
    let apic_id = core::arch::x86_64::__cpuid(1).ebx >> 24;
    let apic_id = (apic_id as usize) & (MAX_CPUS - 1);
    let smbase = SMM_RELOCATION_SMBASES[apic_id].load(Ordering::Acquire) as u32;
    if smbase == 0 {
        return;
    }

    let save_state_smbase = default_save_state_smbase_ptr();

    // SAFETY: this runs in SMM from the default save-state window while SMRAM
    // is open. `smm_relocate_trampoline()` serializes all CPUs.
    unsafe { core::ptr::write_unaligned(save_state_smbase, smbase) };
}

fn default_save_state_smbase_ptr() -> *mut u32 {
    let save_state_top = SMM_DEFAULT_SMBASE + SMM_SAVE_STATE_SIZE;
    let revision_addr = (save_state_top - SMM_REVISION_OFFSET_FROM_TOP) as *const u32;

    // SAFETY: during default-SMRAM relocation, the CPU has populated the
    // architectural save state at the default SMBASE.
    let revision = unsafe { core::ptr::read_unaligned(revision_addr) };
    if revision == SMM_EM64T101_REVISION {
        return revision_addr.wrapping_sub(1).cast_mut();
    }

    let legacy_revision_addr = (SMM_DEFAULT_SMBASE + SMM_LEGACY_REVISION_OFFSET) as *const u32;
    // SAFETY: same default save-state area as above. Some emulators expose the
    // legacy/AMD64 SMBASE field at 0xff00 with revision immediately after it.
    let legacy_revision = unsafe { core::ptr::read_unaligned(legacy_revision_addr) };
    if legacy_revision != 0 {
        return legacy_revision_addr.wrapping_sub(1).cast_mut();
    }

    revision_addr.wrapping_sub(1).cast_mut()
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
pub fn current_cpu_index() -> u32 {
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
/// 1. BSP: enable LAPIC, call CPU-driver `pre_mp_init()` hooks
/// 2. BSP: copy SIPI trampoline to low memory (`0x1000`)
/// 3. BSP: send INIT + SIPI to all APs
/// 4. BSP: wait for APs to check in (with timeout)
/// 5. Flight plan:
///    - If SMM: step "install handlers" (BSP), step "relocate" (all, parallel)
///    - Step "cpu_init" (all CPUs identify themselves and run a matching driver)
///    - Step "mailbox loop" (APs park, BSP continues)
/// 6. BSP: `smm.post_smm_init()` (if SMM), CPU-driver `post_mp_init()` hooks
/// 7. Return [`MpHandle`]
/// Return the number of logical CPUs brought online by the most recent MP init.
pub fn online_cpus() -> u16 {
    ONLINE_CPUS.load(Ordering::Acquire) as u16
}

pub fn mp_init(config: &MpConfig<'_>) -> Result<MpHandle, MpError> {
    let max_aps = config.max_cpus.saturating_sub(1);

    fstart_log::info!("mp: initializing {} CPUs", config.max_cpus);

    // --- Step 1: BSP LAPIC setup ---
    let lapic = Lapic::from_msr();
    lapic.enable();
    lapic.setup_virtual_wire(true);

    fstart_log::info!("mp: BSP LAPIC ID = {}", lapic.id());

    // Pre-MP CPU-driver hooks (BSP only).
    pre_mp_cpu_drivers(config.cpu_drivers);
    store_cpu_drivers(config.cpu_drivers);
    CPU_INIT_ERRORS.store(0, Ordering::Release);

    if max_aps == 0 {
        // Single-CPU system.  Still perform the SMM install + relocation path
        // when requested; coreboot also relocates the BSP before enabling
        // global SMIs.
        if let Some(smm) = config.smm {
            fstart_log::info!("mp: single-CPU SMM path");
            if let Some(info) = smm.smm_info() {
                fstart_log::info!("mp: SMM pre init");
                smm.pre_smm_init();
                let Some(image) = config.smm_image else {
                    fstart_log::error!("mp: SMM requested but no SMM image was provided");
                    clear_mp_globals();
                    return Err(MpError::MissingSmmImage);
                };
                fstart_log::info!("mp: installing SMM handlers");
                if smm
                    .install_smm_handlers(&info, config.max_cpus, image)
                    .is_err()
                {
                    clear_mp_globals();
                    return Err(MpError::SmmInstallFailed);
                }
                // First SMI runs the default-SMRAM relocation handler; after
                // post_smm_init() closes/locks SMRAM, the second SMI proves the
                // permanent copied handler is usable.
                fstart_log::info!("mp: SMM relocate #1");
                smm.smm_relocate();
                fstart_log::info!("mp: SMM post init");
                smm.post_smm_init();
                fstart_log::info!("mp: SMM relocate #2");
                smm.smm_relocate();
            } else {
                fstart_log::info!("mp: SMM provider returned no SMRAM info");
            }
        }
        fstart_log::info!("mp: BSP CPU init");
        cpu_init_trampoline();
        if CPU_INIT_ERRORS.load(Ordering::Acquire) != 0 {
            clear_mp_globals();
            return Err(MpError::UnsupportedCpu);
        }
        fstart_log::info!("mp: BSP post MP init");
        post_mp_cpu_drivers(config.cpu_drivers);
        clear_cpu_drivers();
        ONLINE_CPUS.store(1, Ordering::Release);
        return Ok(MpHandle { num_aps: 0 });
    }

    // --- Step 2: Set up global state for APs ---
    AP_COUNT.store(0, Ordering::Release);
    AP_IN_MAILBOX_LOOP.store(0, Ordering::Release);
    SMM_RELOCATION_LOCK.store(false, Ordering::Release);

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
    CPU_INIT_FN.store(cpu_init_trampoline as *const () as usize, Ordering::Release);
    FLIGHT_PLAN[step_count].barrier.store(0, Ordering::Release);
    FLIGHT_PLAN[step_count]
        .cpus_entered
        .store(0, Ordering::Release);
    FLIGHT_PLAN[step_count]
        .ap_fn
        .store(cpu_init_trampoline as *const () as usize, Ordering::Release);
    FLIGHT_PLAN[step_count]
        .bsp_fn
        .store(cpu_init_trampoline as *const () as usize, Ordering::Release);
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
    if let Err(err) = install_sipi_trampoline(max_aps, &lapic) {
        clear_mp_globals();
        return Err(err);
    }

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
    if checked_in < max_aps {
        // Second SIPI (per Intel SDM recommendation).
        lapic.send_sipi_all_but_self(SIPI_VECTOR_PAGE as u8);
        // Wait up to 50 ms per AP.
        let timeout_us = 50_000u64 * max_aps as u64;
        let mut elapsed = 0u64;
        while (AP_COUNT.load(Ordering::Acquire) as u16) < max_aps && elapsed < timeout_us {
            delay_us(50);
            elapsed += 50;
        }
    }

    let final_count = AP_COUNT.load(Ordering::Acquire) as u16;
    fstart_log::info!("mp: {}/{} APs checked in", final_count, max_aps);

    if final_count == 0 {
        clear_mp_globals();
        return Err(MpError::NoApsResponded);
    }

    // Install SMM handlers only after APs have checked in and are blocked at
    // the first flight-plan step.  This matches coreboot's sequencing: load
    // permanent handlers, then let every CPU enter SMM to relocate SMBASE.
    if let (Some(smm), Some(info)) = (config.smm, smm_info) {
        smm.pre_smm_init();
        let Some(image) = config.smm_image else {
            fstart_log::error!("mp: SMM requested but no SMM image was provided");
            clear_mp_globals();
            return Err(MpError::MissingSmmImage);
        };
        if smm
            .install_smm_handlers(&info, config.max_cpus, image)
            .is_err()
        {
            clear_mp_globals();
            return Err(MpError::SmmInstallFailed);
        }
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

    if CPU_INIT_ERRORS.load(Ordering::Acquire) != 0 {
        clear_mp_globals();
        return Err(MpError::UnsupportedCpu);
    }

    // --- Step 6: Post-init ---
    clear_smm_ops();
    post_mp_cpu_drivers(config.cpu_drivers);
    clear_cpu_drivers();

    ONLINE_CPUS.store((final_count + 1) as usize, Ordering::Release);
    fstart_log::info!("mp: initialization complete ({} CPUs)", final_count + 1);

    if final_count < max_aps {
        fstart_log::warn!(
            "mp: fewer APs than max responded (expected max {}, actual {})",
            max_aps,
            final_count
        );
    }

    Ok(MpHandle {
        num_aps: final_count,
    })
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
fn install_sipi_trampoline(max_aps: u16, _lapic: &Lapic) -> Result<(), MpError> {
    if max_aps as usize > MAX_CPUS || sipi_blob::TRAMPOLINE.len() > 4096 {
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

/// Park any APs brought up by [`mp_init`] before handing control to a payload or OS.
///
/// `mp_init` leaves APs in the mailbox loop so firmware can run scoped work on
/// them.  An OS does not know about that mailbox and expects APs to be quiescent
/// until it sends its own INIT/SIPI sequence.  Call this immediately before a
/// final payload jump.
pub fn park_aps_for_payload() {
    let num_aps = AP_COUNT.load(Ordering::Acquire).min(MAX_CPUS);
    if num_aps == 0 {
        return;
    }

    for i in 0..num_aps {
        let mb = &MAILBOXES[i];
        mb.data.store(0, Ordering::Relaxed);
        mb.func
            .store(park_cpu as *const () as usize, Ordering::Release);
    }

    delay_us(1000);
    fstart_log::info!("mp: {} APs parked for payload handoff", num_aps as u32);
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
fn delay_us(us: u64) {
    crate::x86::udelay(us.min(u32::MAX as u64) as u32);
}
