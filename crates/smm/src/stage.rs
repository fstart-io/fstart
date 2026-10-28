#[cfg(target_os = "none")]
use core::panic::PanicInfo;

pub use crate::SmmEntryParams;
#[cfg(not(test))]
use crate::debug_trace;
use crate::{SmmContext, SmmHandler, enter_rendezvous, leave_rendezvous};

/// Selected-board SMM binding supplied by the board crate.
pub trait SmmStageBoard {
    /// Fully composed platform + board SMM handler.
    type Handler: SmmHandler;
}

/// Declare a board-owned SMM handler entry.
#[macro_export]
macro_rules! smm_bin {
    ($board:ty, $handler:ty) => {
        impl $crate::SmmStageBoard for $board {
            type Handler = $handler;
        }

        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn fstart_smm_handler(params: *mut $crate::SmmEntryParams) {
            // SAFETY: the SMM entry stub passes its loader-filled params.
            unsafe { $crate::handle::<$board>(params) }
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

/// Dispatch one SMI through the selected board's handler.
///
/// Force-inlined into the board's `fstart_smm_handler` so the installed image
/// contains no cross-crate PLT/GOT calls.
///
/// # Safety
///
/// `params` must be the current CPU's valid entry block while executing in SMM.
#[inline(always)]
pub unsafe fn handle<B: SmmStageBoard>(params: *mut SmmEntryParams) {
    let Some(mut ctx) = (unsafe { SmmContext::from_raw(params) }) else {
        return;
    };
    // SAFETY: the installer wrote this handler's own configuration type.
    let config = unsafe { ctx.handler_config::<<B::Handler as SmmHandler>::Config>() };

    if let Some(config) = config {
        // Local MCA state must be observed even by CPUs that will only wait
        // for the shared owner. Never require a missing CPU to enter SMM.
        unsafe { B::Handler::enter_cpu(&mut ctx, config) };
    }

    if !unsafe { enter_rendezvous(&ctx) } {
        #[cfg(not(test))]
        unsafe {
            debug_trace(ctx.cpu())
        };
        return;
    }

    unsafe {
        if let Some(config) = config {
            B::Handler::handle(&mut ctx, config);
        }
        #[cfg(not(test))]
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
    struct Board;
    struct Handler;
    impl SmmStageBoard for Board {
        type Handler = Handler;
    }
    impl SmmHandler for Handler {
        type Config = ();
        unsafe fn enter_cpu(_: &mut SmmContext<'_>, _: &()) {
            LOCAL_ENTERED.store(true, Ordering::Release);
        }
        unsafe fn handle(_: &mut SmmContext<'_>, _: &()) {}
    }

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
        let base = core::ptr::addr_of_mut!(fixture) as usize;
        fixture.runtime.smram_base = base as u64;
        fixture.runtime.smram_size = core::mem::size_of::<Fixture>() as u64;
        fixture.runtime.owner.store(1, Ordering::Release);
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
                unsafe { handle::<Board>(&mut params) };
            });
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            while !LOCAL_ENTERED.load(Ordering::Acquire) && std::time::Instant::now() < deadline {
                std::thread::yield_now();
            }
            let entered_while_owned = LOCAL_ENTERED.load(Ordering::Acquire);
            // Always release, even if the test fails, so the worker can finish.
            fixture.runtime.owner.store(0, Ordering::Release);
            waiter.join().unwrap();
            assert!(entered_while_owned);
        });
    }
}

#[cfg(target_os = "none")]
#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {}
}
