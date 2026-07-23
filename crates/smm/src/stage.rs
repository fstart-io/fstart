#[cfg(target_os = "none")]
use core::panic::PanicInfo;

use crate::{
    debug_trace, obtain_handler_lock, release_handler_lock, wait_for_handler_unlock, SmmContext,
    SmmHandler, SMM_PLATFORM_NONE,
};

pub use crate::SmmEntryParams;

#[repr(align(16))]
#[allow(dead_code)]
struct HeapStore([u8; 4096]);

#[no_mangle]
static _FSTART_HEAP: HeapStore = HeapStore([0; 4096]);

#[no_mangle]
static _FSTART_HEAP_SIZE: usize = 4096;

/// Selected-board SMM binding supplied by the board crate.
pub trait SmmStageBoard {
    /// Runtime platform kind accepted by this board's handler.
    const PLATFORM_KIND: u32;
    /// Fully composed platform + board SMM handler.
    type Handler: SmmHandler;
}

/// Declare a board-owned SMM handler entry.
#[macro_export]
macro_rules! smm_bin {
    ($board:ty, $platform_kind:expr, $handler:ty) => {
        impl $crate::SmmStageBoard for $board {
            const PLATFORM_KIND: u32 = $platform_kind;
            type Handler = $handler;
        }

        #[no_mangle]
        pub unsafe extern "C" fn fstart_smm_handler(params: *mut $crate::SmmEntryParams) {
            // SAFETY: the SMM image trampoline provides the raw entry params.
            unsafe { $crate::handle::<$board>(params) }
        }

        #[used]
        #[cfg_attr(target_os = "none", link_section = ".fstart.keep")]
        static FSTART_SMM_KEEP: unsafe extern "C" fn(*mut $crate::SmmEntryParams) =
            fstart_smm_handler;
    };
}

/// Dispatch one SMM entry through the selected board's handler.
///
/// # Safety
///
/// `params` must be a valid pointer to the SMM entry parameter block provided
/// by the SMM trampoline for the current CPU, or null to indicate no work. The
/// caller must invoke this only while executing in SMM with the expected CPU and
/// platform state for `B`.
pub unsafe fn handle<B: SmmStageBoard>(params: *mut SmmEntryParams) {
    unsafe {
        let Some(mut ctx) = SmmContext::from_raw(params) else {
            return;
        };

        ctx.record_entry();

        if let Some(runtime) = ctx.runtime_mut() {
            if !obtain_handler_lock(runtime) {
                wait_for_handler_unlock(runtime);
                debug_trace(ctx.params.cpu);
                return;
            }
        }

        match ctx.params.platform_kind {
            SMM_PLATFORM_NONE => {}
            kind if kind == B::PLATFORM_KIND => B::Handler::handle(&mut ctx),
            _ => {}
        }

        debug_trace(ctx.params.cpu);

        if let Some(runtime) = ctx.runtime_mut() {
            release_handler_lock(runtime);
        }
    }
}

#[cfg(target_os = "none")]
#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {}
}
