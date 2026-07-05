#![no_std]

extern crate fstart_alloc;

#[cfg(target_os = "none")]
use core::panic::PanicInfo;

use fstart_smm_runtime::{
    debug_trace, obtain_handler_lock, release_handler_lock, wait_for_handler_unlock, SmmContext,
    SmmHandler, SMM_PLATFORM_NONE,
};

pub use fstart_smm_runtime::SmmEntryParams;

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
