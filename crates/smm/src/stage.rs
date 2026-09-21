#[cfg(target_os = "none")]
use core::panic::PanicInfo;

pub use crate::SmmEntryParams;
use crate::{SmmContext, SmmHandler, debug_trace, enter_rendezvous, leave_rendezvous};

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

    if !unsafe { enter_rendezvous(&ctx) } {
        unsafe { debug_trace(ctx.cpu()) };
        return;
    }

    unsafe {
        if let Some(config) = config {
            B::Handler::handle(&mut ctx, &config);
        }
        debug_trace(ctx.cpu());
        leave_rendezvous(&ctx);
    }
}

#[cfg(target_os = "none")]
#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {}
}
