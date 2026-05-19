#![no_std]
#![no_main]

#[cfg(target_os = "none")]
use core::panic::PanicInfo;

#[cfg(smm_platform = "pineview-ich7")]
use fstart_driver_intel_ich7::smm::Ich7SmmHandler;
#[cfg(smm_platform = "qemu-q35")]
use fstart_driver_intel_ich8::smm::Ich8SmmHandler;
#[cfg(smm_platform = "lenovo-x61")]
use fstart_driver_intel_ich8::smm::Ich8SmmHandler;
#[cfg(smm_platform = "lenovo-x61")]
use fstart_mainboard_lenovo_x61::smm::LenovoX61SmmHandler;
use fstart_smm_runtime::{
    debug_trace, obtain_handler_lock, release_handler_lock, wait_for_handler_unlock,
    NoBoardSmmHandler, SmmContext, SmmEntryParams, SmmHandler, SMM_PLATFORM_INTEL_ICH,
    SMM_PLATFORM_NONE,
};

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

#[cfg(smm_platform = "pineview-ich7")]
unsafe fn dispatch_intel_ich(ctx: &mut SmmContext<'_>) {
    Ich7SmmHandler::<NoBoardSmmHandler>::handle(ctx);
}

#[cfg(smm_platform = "qemu-q35")]
unsafe fn dispatch_intel_ich(ctx: &mut SmmContext<'_>) {
    Ich8SmmHandler::<NoBoardSmmHandler>::handle(ctx);
}

#[cfg(smm_platform = "lenovo-x61")]
unsafe fn dispatch_intel_ich(ctx: &mut SmmContext<'_>) {
    Ich8SmmHandler::<LenovoX61SmmHandler>::handle(ctx);
}

#[cfg(target_os = "none")]
#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {}
}
