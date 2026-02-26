//! Hardware driver implementations.
//!
//! Each driver is feature-gated so only the drivers a board needs are compiled.
//! In Rigid mode, unused drivers are completely eliminated.
//!
//! Drivers implement the `Device` trait (from `fstart-services`) with a typed
//! `Config` associated type, and one or more service traits (`Console`,
//! `BlockDevice`, `Timer`).
//!
//! The [`DriverInstance`] enum aggregates all driver configs into a single
//! serde-enabled type.  RON board files use the enum variants directly so
//! the config shape is validated at parse time — no flat bag of `Option`s.
//!
//! See [docs/driver-model.md](../../docs/driver-model.md) for the full
//! driver model architecture.

#![no_std]

pub mod i2c;
pub mod uart;

// ---------------------------------------------------------------------------
// DriverMeta — static metadata about a driver, used by codegen
// ---------------------------------------------------------------------------

/// Static metadata about a driver.
///
/// Returned by [`DriverInstance::meta()`] to give codegen everything it
/// needs to emit imports, construct devices, and generate accessors
/// without per-driver match arms in the stage generator.
#[derive(Debug, Clone, Copy)]
pub struct DriverMeta {
    /// RON / feature-flag name (e.g., `"ns16550"`).
    pub name: &'static str,
    /// Rust type name of the driver struct (e.g., `"Ns16550"`).
    pub type_name: &'static str,
    /// Full module path to import from (e.g., `"fstart_drivers::uart::ns16550"`).
    pub module_path: &'static str,
    /// Rust type name of the config struct (e.g., `"Ns16550Config"`).
    pub config_type: &'static str,
    /// Service traits this driver implements.
    pub services: &'static [&'static str],
    /// Compatible strings for FDT generation.
    pub compatible: &'static [&'static str],
}

// ---------------------------------------------------------------------------
// DriverInstance — typed enum of all known driver configs
// ---------------------------------------------------------------------------

/// A driver instance with its typed configuration.
///
/// Each variant carries the driver's own `Config` struct — the same type
/// that `Device::new()` takes.  This is the Rust-idiomatic equivalent of
/// U-Boot's per-driver `struct xxx_plat`: each driver defines what fields
/// it needs, and the framework validates at parse time (not via `void *`
/// casts at runtime).
///
/// Variants are feature-gated to match the driver modules.  On the host
/// (codegen), enable `all-drivers` to parse any board config.  On the
/// target, only the drivers the board actually uses are compiled in.
///
/// For Flexible mode, unsupported variants produce a clear error at
/// deserialization time rather than a silent mismatch.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum DriverInstance {
    /// NS16550(A) UART — QEMU virt (RISC-V), many x86 platforms.
    #[cfg(feature = "ns16550")]
    Ns16550(uart::ns16550::Ns16550Config),

    /// ARM PL011 UART — QEMU virt (AArch64).
    #[cfg(feature = "pl011")]
    Pl011(uart::pl011::Pl011Config),

    /// Synopsys DesignWare APB I2C controller.
    #[cfg(feature = "designware-i2c")]
    DesignwareI2c(i2c::designware::DesignwareI2cConfig),
}

impl DriverInstance {
    /// Static metadata for this driver variant.
    pub fn meta(&self) -> &'static DriverMeta {
        match self {
            #[cfg(feature = "ns16550")]
            Self::Ns16550(_) => &DriverMeta {
                name: "ns16550",
                type_name: "Ns16550",
                module_path: "fstart_drivers::uart::ns16550",
                config_type: "Ns16550Config",
                services: &["Console"],
                compatible: &["ns16550a", "ns16550"],
            },
            #[cfg(feature = "pl011")]
            Self::Pl011(_) => &DriverMeta {
                name: "pl011",
                type_name: "Pl011",
                module_path: "fstart_drivers::uart::pl011",
                config_type: "Pl011Config",
                services: &["Console"],
                compatible: &["arm,pl011", "pl011"],
            },
            #[cfg(feature = "designware-i2c")]
            Self::DesignwareI2c(_) => &DriverMeta {
                name: "designware-i2c",
                type_name: "DesignwareI2c",
                module_path: "fstart_drivers::i2c::designware",
                config_type: "DesignwareI2cConfig",
                services: &["I2cBus"],
                compatible: &["snps,designware-i2c", "dw-apb-i2c"],
            },
        }
    }

    /// The cargo feature / RON driver name for this variant.
    pub fn driver_name(&self) -> &'static str {
        self.meta().name
    }

    /// Serialize just the inner config struct via the given serializer.
    ///
    /// This enables generic config-to-tokens conversion in `fstart-codegen`
    /// without per-driver match arms there.  Each arm here is a trivial
    /// delegation — the field-level work is handled by the derived
    /// `Serialize` impl on each config struct.
    pub fn serialize_config<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        match self {
            #[cfg(feature = "ns16550")]
            Self::Ns16550(cfg) => serde::Serialize::serialize(cfg, ser),
            #[cfg(feature = "pl011")]
            Self::Pl011(cfg) => serde::Serialize::serialize(cfg, ser),
            #[cfg(feature = "designware-i2c")]
            Self::DesignwareI2c(cfg) => serde::Serialize::serialize(cfg, ser),
        }
    }
}
