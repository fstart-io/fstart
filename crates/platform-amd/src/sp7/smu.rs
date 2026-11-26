// SPDX-License-Identifier: MIT OR Apache-2.0
//! SMU v14 at SP7's boot points: the feature words the SMU reports and the
//! platform overrides (`SmuFixupPlatformConfigSP7`), `SmuInitialization`
//! on every socket after MPIO training (the vendor's AmdNbioDxe entry, with
//! the board's [`super::Config::smu`]), and the unused-tile clock gating
//! both `EnableSPGUnusedPcie` and `MPIO_ENABLE_CG` take.
//! Generic over the SoC: its strings are its own.

use fstart_arch::x86::cpu::msr::NativeMsr;
use fstart_driver_amd::smu::v14::{features, init};

use super::init::State;
use super::{Sp7, Variant};
use crate::flow::Ctx;

/// Socket 0's feature defaults (`11h`, never the emulation request) with
/// the board's overrides; `None` when the SMU does not answer.
fn feature_controls<V: Variant>(
    ctx: &Ctx<'_, Sp7<V>>,
    state: &State,
) -> Option<features::Controls> {
    match features::defaults(&state.smu(ctx.clock), false) {
        Ok(defaults) => Some(defaults.with(&ctx.config.smu.overrides)),
        Err(_) => {
            fstart_log::warn!("{}: SMU feature defaults (11h) failed", V::NAME);
            None
        }
    }
}

/// `PcdCtrlUnusedTileClkGating` as the vendor uses it (AmdNbioDxe
/// `0x4e70`): off when APOB `7/3` says DRAM survived a warm reset.
pub(super) fn unused_tile_clock_gating<V: Variant>(ctx: &Ctx<'_, Sp7<V>>) -> bool {
    let preserved = (ctx.hw.apob().ok())
        .and_then(|apob| apob.boot_info().ok())
        .is_some_and(|info| info.dram_survives_warm_reset != 0);
    ctx.config.nbio.unused_tile_clock_gating && !preserved
}

/// `SmuInitialization` on every socket's SMU: the PPTable, the AGM log and
/// the optional SMU buffer in the firmware's reserved DRAM
/// (`crate::memory::dma_alloc`), then the messages of
/// `smu::v14::init::initialize`. A socket whose SMU refuses a message is
/// reported, the boot goes on. Returns the feature words sent, which the
/// ready-to-boot steps reuse as the vendor's AmdNbioDxe does
/// ([`mpio_clock_gating`]); `None` when the SMU did not report them.
pub(super) fn initialize<V: Variant>(
    ctx: &Ctx<'_, Sp7<V>>,
    state: &State,
) -> Option<features::Controls> {
    let policy = &ctx.config.smu;
    let controls = feature_controls(ctx, state)?;
    // SAFETY: the BSP reads its own MSR.
    let msr = unsafe { NativeMsr::new() };
    let drb = features::drb_status_cleared(&msr);
    let unused_tile = unused_tile_clock_gating(ctx);
    // The APCB's NPS token, which the vendor reads; the ABL's actual NPS
    // has the same encoding (3 = NPS4), 2 is the vendor's fallback.
    let nps = (ctx.hw.apob().ok())
        .and_then(|apob| apob.nps().ok())
        .unwrap_or(2);
    for socket in 0..state.sockets() {
        let Some(buffers) = buffers(policy) else {
            fstart_log::error!("{}: S{} SMU buffers not allocated", V::NAME, socket);
            return Some(controls);
        };
        let smu = state.smu_on(socket, 0, ctx.clock);
        match init::initialize(&smu, policy, &buffers, controls, drb, unused_tile, nps) {
            Ok(()) => fstart_log::info!(
                "{}: S{} SMU initialized, features {:#x}/{:#x}/{:#x}",
                V::NAME,
                socket,
                controls.standard,
                controls.extended,
                controls.standard64
            ),
            Err((id, _)) => {
                fstart_log::error!("{}: S{} SMU message {:#x} refused", V::NAME, socket, id)
            }
        }
    }
    Some(controls)
}

/// One socket's PPTable copy, AGM log and SMU buffer.
fn buffers(policy: &init::Policy) -> Option<init::Buffers> {
    use zerocopy::IntoBytes;
    let table = policy.pptable.as_bytes();
    let pptable = crate::memory::dma_alloc(table.len(), 0x1000).ok()?;
    pptable.copy_from_slice(table);
    // The vendor writes the table back before `10h` (`0x19000`).
    for line in pptable.chunks(64) {
        // SAFETY: the bytes just written, in mapped DRAM.
        unsafe { fstart_arch::x86::flush_cache_line(line.as_ptr()) };
    }
    let agm_log = crate::memory::dma_alloc(policy.agm_log_bytes(), 0x1000).ok()?;
    let dram_buffer = match policy.dram_buffer_mib {
        0 => None,
        mib => {
            let bytes = (mib as usize) << 20;
            Some(crate::memory::dma_alloc(bytes, 0x10_0000).ok()?.as_ptr() as u64)
        }
    };
    Some(init::Buffers {
        pptable: pptable.as_ptr() as u64,
        agm_log: agm_log.as_ptr() as u64,
        dram_buffer,
    })
}

/// The `MPIO_ENABLE_CG` arguments (`SmuMpioClockGatingRTBCallBack`):
/// feature bits 20 and 14 of the words [`initialize`] sent (the vendor's
/// AmdNbioDxe computes them once, at its entry; no second `11h`), the
/// unused-tile clock gating; all off when the SMU did not report its
/// features.
pub(super) fn mpio_clock_gating<V: Variant>(ctx: &Ctx<'_, Sp7<V>>, state: &State) -> [bool; 3] {
    let unused_tile = unused_tile_clock_gating(ctx);
    state
        .smu_features()
        .map_or([false, false, unused_tile], |c| {
            c.mpio_clock_gating(unused_tile)
        })
}
