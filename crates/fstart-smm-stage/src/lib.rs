#![no_std]
#![no_main]

extern crate fstart_alloc;

#[cfg(target_os = "none")]
use core::panic::PanicInfo;

#[cfg(smm_platform = "lenovo-x61")]
use fstart_driver_intel_ich8::smm::Ich8SmmHandler;
use fstart_smm_runtime::{
    debug_trace, obtain_handler_lock, release_handler_lock, wait_for_handler_unlock,
    NoBoardSmmHandler, SmmContext, SmmEntryParams, SmmHandler, SMM_PLATFORM_INTEL_ICH,
    SMM_PLATFORM_NONE,
};

#[repr(align(16))]
#[allow(dead_code)]
struct HeapStore([u8; 4096]);

#[no_mangle]
static _FSTART_HEAP: HeapStore = HeapStore([0; 4096]);

#[no_mangle]
static _FSTART_HEAP_SIZE: usize = 4096;

/// SMM entry point called by the assembly/runtime trampoline.
///
/// # Safety
///
/// `params` must be a valid pointer to the SMM entry parameter block provided
/// by the SMM trampoline for the current CPU, or null to indicate no work. The
/// caller must invoke this only while executing in SMM with the expected CPU and
/// platform state.
#[no_mangle]
pub unsafe extern "C" fn fstart_smm_handler(params: *mut SmmEntryParams) {
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
            SMM_PLATFORM_INTEL_ICH => dispatch_intel_ich(&mut ctx),
            _ => {}
        }

        debug_trace(ctx.params.cpu);

        if let Some(runtime) = ctx.runtime_mut() {
            release_handler_lock(runtime);
        }
    }
}

#[cfg(smm_platform = "lenovo-x61")]
unsafe fn dispatch_intel_ich(ctx: &mut SmmContext<'_>) {
    // SAFETY: caller guarantees SMM entry context per SmmHandler::handle contract.
    unsafe { Ich8SmmHandler::<NoBoardSmmHandler>::handle(ctx) };
}

#[cfg(target_os = "none")]
#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {}
}
