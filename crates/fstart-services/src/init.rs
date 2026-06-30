//! Generic platform initialization phase service traits.
//!
//! These traits model firmware sequencing phases without encoding a
//! particular chipset topology such as northbridge/southbridge or PCH.  A
//! board stage selects the devices that participate in each phase via Rust
//! board/build metadata and flow profiles.

use core::marker::PhantomData;

use crate::ServiceError;

/// Minimal hardware setup required before the console can be initialized.
///
/// Examples include opening clock gates and pinmux on SoCs, enabling x86 ECAM,
/// or opening LPC decode so a SuperIO UART is reachable.
pub trait PreConsoleInit: Send + Sync {
    /// Perform pre-console setup. Implementations must not require logging.
    fn pre_console_init(&mut self) -> Result<(), ServiceError>;
}

/// Logged early platform initialization before DRAM training or bus probing.
///
/// Examples include chipset BAR setup, GPIO routing, SMBus enablement, PLL
/// setup, and other work that benefits from console diagnostics.
pub trait EarlyInit: Send + Sync {
    /// Perform early platform initialization. Must be safe to call once per
    /// stage that declares the capability.
    fn early_init(&mut self) -> Result<(), ServiceError>;
}

/// Rebuild stage-local software bindings to already-programmed hardware.
///
/// Multi-stage firmware has a fresh BSS in every stage.  Drivers can use this
/// hook to rebind global accessors, cached MMIO base addresses, or other
/// per-stage software state without repeating heavyweight hardware init.
pub trait StageLocalInit: Send + Sync {
    /// Rebuild stage-local state. Must be idempotent.
    fn stage_local_init(&mut self) -> Result<(), ServiceError>;
}

/// DRAM-backed platform/device initialization after memory is usable.
pub trait PostDramInit: Send + Sync {
    /// Perform post-DRAM setup.
    fn post_dram_init(&mut self) -> Result<(), ServiceError>;
}

/// Final platform lockdown before handing control to the payload.
pub trait FinalizeInit: Send + Sync {
    /// Lock write-once/security-sensitive hardware state.
    fn finalize_init(&mut self) -> Result<(), ServiceError>;
}

/// Context shared across step-based hardware initialization methods.
///
/// The initial context is deliberately small. Board-owned stage adapters can
/// extend this with generic device lookup and stage-local services without
/// adding chipset-specific helpers to common code.
pub struct InitContext<'stage> {
    _stage: PhantomData<&'stage mut ()>,
}

impl<'stage> InitContext<'stage> {
    /// Construct an empty initialization context.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            _stage: PhantomData,
        }
    }
}

impl Default for InitContext<'_> {
    fn default() -> Self {
        Self::new()
    }
}

/// Step-based hardware initialization lifecycle.
///
/// Drivers implement only the steps where they participate. Default no-op
/// methods make static typed board containers cheap: monomorphized release
/// builds can inline and remove calls to steps a concrete driver does not use.
pub trait HardwareInit {
    /// Run board/SoC operations that must happen before clocks or console.
    #[inline(always)]
    fn very_early(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        Ok(())
    }

    /// Enable clocks/resets needed by pinmux, UART, timers, and DRAM setup.
    #[inline(always)]
    fn early_clocks(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        Ok(())
    }

    /// Route pins for early UART, DRAM, boot media, and board straps.
    #[inline(always)]
    fn pinmux(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        Ok(())
    }

    /// Decode/register setup required before a console driver can work.
    #[inline(always)]
    fn pre_console(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        Ok(())
    }

    /// Initialize the selected boot console.
    #[inline(always)]
    fn console(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        Ok(())
    }

    /// Run diagnostics and safety checks after logging is available.
    #[inline(always)]
    fn post_console(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        Ok(())
    }

    /// Read SPD, straps, or board config needed for memory init.
    #[inline(always)]
    fn memory_discovery(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        Ok(())
    }

    /// Train/enable DRAM or validate fixed RAM setup.
    #[inline(always)]
    fn dram(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        Ok(())
    }

    /// Run DRAM-backed platform/device setup.
    #[inline(always)]
    fn post_dram(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        Ok(())
    }

    /// Enable fixed bridges/resources before probing children.
    #[inline(always)]
    fn bus_early(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        Ok(())
    }

    /// Discover or instantiate child devices on enumerable buses.
    #[inline(always)]
    fn bus_probe(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        Ok(())
    }

    /// Run hooks after generic drivers are available.
    #[inline(always)]
    fn drivers_ready(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        Ok(())
    }

    /// Bring up boot media and firmware-volume access.
    #[inline(always)]
    fn storage(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        Ok(())
    }

    /// Measure or verify firmware and policy inputs.
    #[inline(always)]
    fn security(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        Ok(())
    }

    /// Let devices participate in payload loading.
    #[inline(always)]
    fn payload_load(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        Ok(())
    }

    /// Finalize handoff tables and quiesce firmware-owned devices.
    #[inline(always)]
    fn handoff(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        Ok(())
    }
}

impl HardwareInit for () {}

// Static board crates often need a small ordered set of concrete devices for a
// stage. Tuple forwarding gives them parent-before-child style composition
// without a board-local `impl HardwareInit` that repeats every flow step, and
// without using dynamic dispatch that would keep no-op methods alive.
macro_rules! impl_hardware_init_tuple {
    ($($field:tt:$name:ident),+ $(,)?) => {
        impl<$($name),+> HardwareInit for ($($name,)+)
        where
            $($name: HardwareInit,)+
        {
            #[inline(always)]
            fn very_early(&mut self, ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
                $(self.$field.very_early(ctx)?;)+
                Ok(())
            }

            #[inline(always)]
            fn early_clocks(&mut self, ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
                $(self.$field.early_clocks(ctx)?;)+
                Ok(())
            }

            #[inline(always)]
            fn pinmux(&mut self, ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
                $(self.$field.pinmux(ctx)?;)+
                Ok(())
            }

            #[inline(always)]
            fn pre_console(&mut self, ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
                $(self.$field.pre_console(ctx)?;)+
                Ok(())
            }

            #[inline(always)]
            fn console(&mut self, ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
                $(self.$field.console(ctx)?;)+
                Ok(())
            }

            #[inline(always)]
            fn post_console(&mut self, ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
                $(self.$field.post_console(ctx)?;)+
                Ok(())
            }

            #[inline(always)]
            fn memory_discovery(&mut self, ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
                $(self.$field.memory_discovery(ctx)?;)+
                Ok(())
            }

            #[inline(always)]
            fn dram(&mut self, ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
                $(self.$field.dram(ctx)?;)+
                Ok(())
            }

            #[inline(always)]
            fn post_dram(&mut self, ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
                $(self.$field.post_dram(ctx)?;)+
                Ok(())
            }

            #[inline(always)]
            fn bus_early(&mut self, ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
                $(self.$field.bus_early(ctx)?;)+
                Ok(())
            }

            #[inline(always)]
            fn bus_probe(&mut self, ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
                $(self.$field.bus_probe(ctx)?;)+
                Ok(())
            }

            #[inline(always)]
            fn drivers_ready(&mut self, ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
                $(self.$field.drivers_ready(ctx)?;)+
                Ok(())
            }

            #[inline(always)]
            fn storage(&mut self, ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
                $(self.$field.storage(ctx)?;)+
                Ok(())
            }

            #[inline(always)]
            fn security(&mut self, ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
                $(self.$field.security(ctx)?;)+
                Ok(())
            }

            #[inline(always)]
            fn payload_load(&mut self, ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
                $(self.$field.payload_load(ctx)?;)+
                Ok(())
            }

            #[inline(always)]
            fn handoff(&mut self, ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
                $(self.$field.handoff(ctx)?;)+
                Ok(())
            }
        }
    };
}

impl_hardware_init_tuple!(0:A);
impl_hardware_init_tuple!(0:A, 1:B);
impl_hardware_init_tuple!(0:A, 1:B, 2:C);
impl_hardware_init_tuple!(0:A, 1:B, 2:C, 3:D);

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Default)]
    struct CountingDevice {
        pre_console_calls: u8,
        post_console_calls: u8,
        fail_pre_console: bool,
    }

    impl CountingDevice {
        const fn failing_pre_console() -> Self {
            Self {
                pre_console_calls: 0,
                post_console_calls: 0,
                fail_pre_console: true,
            }
        }
    }

    impl HardwareInit for CountingDevice {
        fn pre_console(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
            self.pre_console_calls += 1;
            if self.fail_pre_console {
                Err(ServiceError::HardwareError)
            } else {
                Ok(())
            }
        }

        fn post_console(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
            self.post_console_calls += 1;
            Ok(())
        }
    }

    #[test]
    fn tuple_hardware_init_forwards_steps_in_order() {
        let mut devices = (CountingDevice::default(), CountingDevice::default());
        let mut ctx = InitContext::new();

        devices.pre_console(&mut ctx).unwrap();
        devices.post_console(&mut ctx).unwrap();

        assert_eq!(devices.0.pre_console_calls, 1);
        assert_eq!(devices.1.pre_console_calls, 1);
        assert_eq!(devices.0.post_console_calls, 1);
        assert_eq!(devices.1.post_console_calls, 1);
    }

    #[test]
    fn tuple_hardware_init_stops_on_first_error() {
        let mut devices = (
            CountingDevice::default(),
            CountingDevice::failing_pre_console(),
            CountingDevice::default(),
        );
        let mut ctx = InitContext::new();

        assert_eq!(
            devices.pre_console(&mut ctx),
            Err(ServiceError::HardwareError)
        );
        assert_eq!(devices.0.pre_console_calls, 1);
        assert_eq!(devices.1.pre_console_calls, 1);
        assert_eq!(devices.2.pre_console_calls, 0);
    }
}
