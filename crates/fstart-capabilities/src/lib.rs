//! Composable capability modules for firmware stages.
//!
//! Each capability is a unit of firmware functionality. The board RON file
//! declares which capabilities run in which order. The fstart-stage build.rs
//! generates a `fstart_main()` that calls them in sequence.
//!
//! Capability functions take a mutable `StageContext` which provides access
//! to the console and other services initialized by earlier capabilities.

#![no_std]

use fstart_services::Console;

/// Context passed through capability execution.
/// Accumulates services as capabilities initialize them.
pub struct StageContext<C: Console> {
    pub console: Option<C>,
}

impl<C: Console> StageContext<C> {
    pub const fn new() -> Self {
        Self { console: None }
    }
}

/// Initialize a console device.
///
/// The generated code calls this with the concrete driver type and resources
/// determined from the board RON. The generic parameters are filled in at
/// codegen time so this compiles to direct, non-generic code.
pub fn console_init<C: Console>(ctx: &mut StageContext<C>, console: C) {
    let _ = console.write_line("[fstart] console initialized");
    ctx.console = Some(console);
}
