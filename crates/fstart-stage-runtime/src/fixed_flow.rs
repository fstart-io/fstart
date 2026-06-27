//! Fixed handwritten stage flow for static typed board crates.
//!
//! This module is the migration target described in the Rust board-builder
//! plan. Unlike [`crate::flow`], it does not replay generated [`crate::StageOp`]
//! tables. The order of semantic barriers is fixed in this Rust function, and
//! board/device participation is expressed by concrete [`HardwareInit`]
//! implementations that optimize away when a device uses default no-op methods.

use fstart_services::{HardwareInit, InitContext, ServiceError};

/// Static typed board contract consumed by the fixed stage flow.
pub trait StaticBoard: Sized {
    /// Concrete device container for this board.
    type Devices: HardwareInit;

    /// Construct the board and its device container.
    fn new() -> Result<Self, ServiceError>;

    /// Mutable access to the device container.
    fn devices_mut(&mut self) -> &mut Self::Devices;

    /// Called if board construction or any flow step fails before a richer
    /// platform-specific panic path is available.
    fn halt() -> !;

    /// Install the console selected by board metadata.
    fn install_console(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        Ok(())
    }

    /// Mount or publish the firmware volume after storage is initialized.
    fn mount_firmware_volume(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        Ok(())
    }

    /// Verify firmware-volume policy inputs.
    fn verify_firmware_volume(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        Ok(())
    }

    /// Select payload/configuration before loading.
    fn select_payload(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        Ok(())
    }

    /// Load the selected payload.
    fn load_payload(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        Ok(())
    }

    /// Verify the loaded payload when security flow is enabled.
    fn verify_loaded_payload(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        Ok(())
    }

    /// Finalize FDT/ACPI/SMBIOS or other handoff metadata.
    fn finalize_handoff(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        Ok(())
    }

    /// Jump to the payload or next firmware stage.
    fn boot_payload(self) -> !;
}

/// Run the fixed handwritten stage sequence for a static typed board.
pub fn run<B: StaticBoard>() -> ! {
    let mut board = match B::new() {
        Ok(board) => board,
        Err(_) => B::halt(),
    };
    let mut ctx = InitContext::new();

    #[cfg(feature = "flow-early-platform-v2")]
    {
        run_step::<B>(&mut board, &mut ctx, |b, ctx| {
            b.devices_mut().very_early(ctx)
        });
        run_step::<B>(&mut board, &mut ctx, |b, ctx| {
            b.devices_mut().early_clocks(ctx)
        });
        run_step::<B>(&mut board, &mut ctx, |b, ctx| b.devices_mut().pinmux(ctx));
        run_step::<B>(&mut board, &mut ctx, |b, ctx| {
            b.devices_mut().pre_console(ctx)
        });
        run_step::<B>(&mut board, &mut ctx, |b, ctx| {
            b.devices_mut().console(ctx)?;
            b.install_console(ctx)
        });
        run_step::<B>(&mut board, &mut ctx, |b, ctx| {
            b.devices_mut().post_console(ctx)
        });
    }

    #[cfg(feature = "flow-memory-v2")]
    {
        run_step::<B>(&mut board, &mut ctx, |b, ctx| {
            b.devices_mut().memory_discovery(ctx)
        });
        run_step::<B>(&mut board, &mut ctx, |b, ctx| b.devices_mut().dram(ctx));
        run_step::<B>(&mut board, &mut ctx, |b, ctx| {
            b.devices_mut().post_dram(ctx)
        });
    }

    #[cfg(feature = "flow-bus-v2")]
    {
        run_step::<B>(&mut board, &mut ctx, |b, ctx| {
            b.devices_mut().bus_early(ctx)
        });
        run_step::<B>(&mut board, &mut ctx, |b, ctx| {
            b.devices_mut().bus_probe(ctx)?;
            b.devices_mut().drivers_ready(ctx)
        });
    }

    #[cfg(feature = "flow-storage-v2")]
    run_step::<B>(&mut board, &mut ctx, |b, ctx| {
        b.devices_mut().storage(ctx)?;
        b.mount_firmware_volume(ctx)
    });

    #[cfg(feature = "flow-security-v2")]
    run_step::<B>(&mut board, &mut ctx, |b, ctx| {
        b.devices_mut().security(ctx)?;
        b.verify_firmware_volume(ctx)
    });

    #[cfg(feature = "flow-payload-v2")]
    {
        run_step::<B>(&mut board, &mut ctx, |b, ctx| b.select_payload(ctx));
        run_step::<B>(&mut board, &mut ctx, |b, ctx| {
            b.devices_mut().payload_load(ctx)?;
            b.load_payload(ctx)
        });

        #[cfg(feature = "flow-security-v2")]
        run_step::<B>(&mut board, &mut ctx, |b, ctx| b.verify_loaded_payload(ctx));
    }

    #[cfg(feature = "flow-handoff-v2")]
    {
        run_step::<B>(&mut board, &mut ctx, |b, ctx| {
            b.devices_mut().handoff(ctx)?;
            b.finalize_handoff(ctx)
        });
        board.boot_payload();
    }

    B::halt()
}

fn run_step<B: StaticBoard>(
    board: &mut B,
    ctx: &mut InitContext<'_>,
    step: impl FnOnce(&mut B, &mut InitContext<'_>) -> Result<(), ServiceError>,
) {
    if step(board, ctx).is_err() {
        B::halt();
    }
}
