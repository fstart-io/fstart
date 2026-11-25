#[cfg(target_os = "none")]
use core::panic::PanicInfo;

pub use crate::SmmEntryParams;
#[cfg(all(not(test), feature = "debugcon"))]
use crate::debug_trace;
use crate::{SmmContext, enter_rendezvous, leave_rendezvous};

/// Declare the board's SMM handler entry.
///
/// `handler` handles one SMI on the CPU that owns the shared dispatch:
/// `fn(&mut SmmContext<'_>, &Config)`, where `Config` is the type the
/// installer wrote into SMRAM (the southbridge or SoC driver's). The
/// optional `enter_cpu`, with the same signature, runs first on every CPU
/// that enters, for CPU-local SMI causes and MSRs; it must not wait for
/// other CPUs or touch unsynchronized shared state.
///
/// ```ignore
/// fn smi(_ctx: &mut SmmContext<'_>, config: &IchSmmConfig) {
///     let smi = unsafe { PendingSmi::take(config) };
///     // board sources
///     smi.finish();
/// }
/// fstart_platform_intel::smm::smm_bin!(handler: smi);
/// ```
#[macro_export]
macro_rules! smm_bin {
    (handler: $handler:path) => {
        $crate::smm_bin!(@entry |_, _| {}, $handler);
    };
    (handler: $handler:path, enter_cpu: $enter_cpu:path) => {
        $crate::smm_bin!(@entry $enter_cpu, $handler);
    };
    (@entry $enter_cpu:expr, $handler:path) => {
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn fstart_smm_handler(params: *mut $crate::SmmEntryParams) {
            // SAFETY: the SMM entry stub passes its loader-filled params.
            unsafe { $crate::handle(params, $enter_cpu, $handler) }
        }
        #[repr(align(16))]
        #[allow(dead_code)]
        struct FstartSmmHeap([u8; 4096]);

        // Emit these roots beside the board's handler entry so archive member
        // selection cannot discard the real BSS tail.
        #[used]
        #[cfg_attr(target_os = "none", unsafe(link_section = ".bss.fstart.heap"))]
        #[unsafe(no_mangle)]
        static _FSTART_HEAP: FstartSmmHeap = FstartSmmHeap([0; 4096]);
        #[used]
        #[cfg_attr(target_os = "none", unsafe(link_section = ".rodata.fstart.heap"))]
        #[unsafe(no_mangle)]
        static _FSTART_HEAP_SIZE: usize = 4096;

        #[used]
        #[cfg_attr(target_os = "none", unsafe(link_section = ".fstart.keep"))]
        static FSTART_SMM_KEEP: unsafe extern "C" fn(*mut $crate::SmmEntryParams) =
            fstart_smm_handler;
    };
}

/// Dispatch one SMI: `enter_cpu` on every entrant, then `handler` on the
/// CPU that wins the rendezvous.
///
/// Force-inlined into the board's `fstart_smm_handler` so the installed image
/// contains no cross-crate PLT/GOT calls.
///
/// # Safety
///
/// `params` must be the current CPU's valid entry block while executing in SMM.
#[inline(always)]
pub unsafe fn handle<C: Copy>(
    params: *mut SmmEntryParams,
    enter_cpu: impl FnOnce(&mut SmmContext<'_>, &C),
    handler: impl FnOnce(&mut SmmContext<'_>, &C),
) {
    let Some(mut ctx) = (unsafe { SmmContext::from_raw(params) }) else {
        return;
    };
    // SAFETY: the installer wrote the configuration the handler takes.
    let config = unsafe { ctx.handler_config::<C>() };

    if let Some(config) = config {
        // Local MCA state must be observed even by CPUs that will only wait
        // for the shared owner. Never require a missing CPU to enter SMM.
        enter_cpu(&mut ctx, config);
    }
    if !unsafe { enter_rendezvous(&ctx) } {
        #[cfg(all(not(test), feature = "debugcon"))]
        unsafe {
            debug_trace(ctx.cpu())
        };
        return;
    }

    // The interrupted OS may be between its CF8 selector write and CFC access.
    #[cfg(all(not(test), any(target_arch = "x86", target_arch = "x86_64")))]
    // SAFETY: SMM owns the CPU and this dispatch; CF8 is a plain I/O register.
    let cf8 = unsafe { fstart_core::pio::inl(fstart_pci::cf8::ADDRESS_PORT) };
    if let Some(config) = config {
        handler(&mut ctx, config);
    }
    #[cfg(all(not(test), any(target_arch = "x86", target_arch = "x86_64")))]
    // SAFETY: restore before releasing rendezvous so the interrupted OS resumes cleanly.
    unsafe {
        fstart_core::pio::outl(fstart_pci::cf8::ADDRESS_PORT, cf8);
    }
    unsafe {
        #[cfg(all(not(test), feature = "debugcon"))]
        debug_trace(ctx.cpu());
        leave_rendezvous(&ctx);
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use crate::SmmRuntime;
    use core::sync::atomic::{AtomicBool, Ordering};
    static LOCAL_ENTERED: AtomicBool = AtomicBool::new(false);
    fn enter_cpu(_: &mut SmmContext<'_>, _: &()) {
        LOCAL_ENTERED.store(true, Ordering::Release);
    }
    fn handler(_: &mut SmmContext<'_>, _: &()) {}

    #[test]
    fn local_work_runs_before_waiting_for_a_different_shared_owner() {
        #[repr(C, align(16))]
        struct Fixture {
            runtime: SmmRuntime,
            padding: [u8; 24],
        }
        let mut fixture = Fixture {
            runtime: SmmRuntime::new(0, 0, 2, 0x400, 48, 0),
            padding: [0; 24],
        };
        // Initialize plain fields before exposing the pointer to another thread
        // (Stacked Borrows: subsequent accesses only touch atomic fields).
        fixture.runtime.smram_size = core::mem::size_of::<Fixture>() as u64;
        // CPU 0 owns generation 1.
        fixture
            .runtime
            .rendezvous
            .store(1 << 32 | 1, Ordering::Release);
        let base_ptr = core::ptr::addr_of_mut!(fixture);
        let base = base_ptr as usize;
        // SAFETY: exclusive access before spawning the waiter thread.
        unsafe { (*base_ptr).runtime.smram_base = base as u64 };
        let fixture = &fixture;
        LOCAL_ENTERED.store(false, Ordering::Relaxed);
        std::thread::scope(|scope| {
            let waiter = scope.spawn(move || {
                let mut params = SmmEntryParams {
                    cpu: 1,
                    runtime: base as u64,
                    stack_size: 0,
                    stack_top: 0,
                    common_entry: 0,
                    coreboot_module_args: 0,
                    cr3: 0,
                    entry_base: 0,
                };
                unsafe { handle(&mut params, enter_cpu, handler) };
            });
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            while !LOCAL_ENTERED.load(Ordering::Acquire) && std::time::Instant::now() < deadline {
                std::thread::yield_now();
            }
            let entered_while_owned = LOCAL_ENTERED.load(Ordering::Acquire);
            // The waiter acknowledges the owner's generation once parked.
            while fixture.runtime.parked.load(Ordering::Acquire) != 1 << 32 | 1
                && std::time::Instant::now() < deadline
            {
                std::thread::yield_now();
            }
            let parked = fixture.runtime.parked.load(Ordering::Acquire);
            // Always release, even if the test fails, so the worker can finish.
            fixture.runtime.rendezvous.store(1 << 32, Ordering::Release);
            waiter.join().unwrap();
            assert!(entered_while_owned);
            assert_eq!(parked, 1 << 32 | 1);
        });
    }
}

#[cfg(target_os = "none")]
#[panic_handler]
#[inline(never)]
fn panic(_info: &PanicInfo) -> ! {
    unsafe extern "C" {
        /// Intentionally undefined: any reachable panic in an SMM handler
        /// fails the `ld --gc-sections` link instead of hanging in SMM.
        fn fstart_smm_panic_is_possible() -> !;
    }
    // SAFETY: Never defined; reachability is a compile-time/link-time error.
    unsafe { fstart_smm_panic_is_possible() }
}
