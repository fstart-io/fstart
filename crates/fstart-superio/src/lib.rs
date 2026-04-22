//! Generic SuperIO chip framework.
//!
//! x86 SuperIO controllers (ITE IT87xx, Winbond / Nuvoton W836xx, SMSC
//! SCH3xxx, ...) follow a common pattern:
//!
//! 1. Enter configuration mode by writing a magic byte sequence to an
//!    index port (commonly `0x2e` or `0x4e`).
//! 2. Select a Logical Device Number (LDN) via config register `0x07`.
//! 3. Program the LDN's registers (`0x30` = enable, `0x60/0x61` = I/O
//!    base, `0x70` = IRQ, ...).
//! 4. Exit configuration mode by writing a chip-specific sequence.
//!
//! Individual chips differ only in:
//! - The enter-config magic sequence (e.g., `[0x87, 0x87]` for ITE,
//!   `[0x87, 0x87]` twice for some W836xx, `[0x55]` for SMSC).
//! - The exit-config register/value (`(0x02, 0x02)` for ITE).
//! - Which LDNs map to which functions (COM1 is LDN 1 on ITE, LDN 2 on
//!   W836xx).
//! - The chip ID reported by reading `0x20/0x21`.
//!
//! The `SuperIoChip` trait captures all of these per-chip details as
//! associated constants. The generic `SuperIo<C>` driver then works
//! uniformly for all chips — a new chip requires only ~25 lines.
//!
//! Board authors never touch LDNs. They describe functions by name
//! ([`ComPortConfig`], [`KbcConfig`], etc.) and the driver maps names
//! to the chip's LDNs via the trait's associated constants.

#![no_std]

use fstart_services::device::{BusDevice, DeviceError};
use serde::{Deserialize, Serialize};

use core::marker::PhantomData;

// ---------------------------------------------------------------------------
// The SuperIoChip trait — per-chip specialization
// ---------------------------------------------------------------------------

/// Per-chip characterization for the generic [`SuperIo`] driver.
///
/// A chip descriptor is a zero-sized type that implements this trait
/// as a bag of associated constants. Each constant tells the generic
/// driver how to talk to this particular chip.
pub trait SuperIoChip: Send + Sync + 'static {
    /// Magic byte sequence that unlocks configuration mode.
    ///
    /// Written in order to the chip's index port. For ITE parts this
    /// is `[0x87, 0x87]`; for Winbond `[0x87, 0x87]` twice is common.
    const ENTER_SEQ: &'static [u8];
    /// Register offset to write for exiting configuration mode.
    const EXIT_REG: u8;
    /// Value to write at [`Self::EXIT_REG`] to exit configuration mode.
    const EXIT_VAL: u8;
    /// Expected chip ID (combined from registers `0x20` and `0x21`).
    ///
    /// `0x20` is the high byte, `0x21` the low byte. Driver init fails
    /// with [`DeviceError::InitFailed`] if the read value does not match.
    const CHIP_ID: u16;

    /// LDN for COM1, if supported.
    const COM1_LDN: Option<u8>;
    /// LDN for COM2, if supported.
    const COM2_LDN: Option<u8>;
    /// LDN for the PS/2 keyboard controller, if supported.
    const KBC_LDN: Option<u8>;
    /// LDN for the PS/2 mouse, if supported.
    const MOUSE_LDN: Option<u8>;
    /// LDN for the embedded controller / environment controller.
    const EC_LDN: Option<u8>;
    /// LDN for GPIO, if supported.
    const GPIO_LDN: Option<u8>;
    /// LDN for consumer IR, if supported.
    const CIR_LDN: Option<u8>;
    /// LDN for the parallel port, if supported.
    const PARALLEL_LDN: Option<u8>;

    /// Optional chip-specific init hook, invoked inside config mode
    /// after the ID check and before any LDN is configured.
    fn chip_init(_base_port: u16) {}

    /// Optional override byte appended to the enter sequence.
    ///
    /// ITE parts use `0x55` for port 0x2E and `0xAA` for port 0x4E as
    /// the fourth byte. Chips that don't need this return `None`.
    fn enter_last_byte(base_port: u16) -> Option<u8> {
        let _ = base_port;
        None
    }
}

// ---------------------------------------------------------------------------
// Config types
// ---------------------------------------------------------------------------

/// Top-level SuperIO config shared by every chip.
///
/// Board authors enable individual functions by setting the matching
/// field to `Some(...)`. The base port (e.g., `0x2e`) comes from the
/// device's `bus: Lpc(0x2e)` attachment, not from this config.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SuperIoConfig {
    /// Primary UART (COM1).
    #[serde(default)]
    pub com1: Option<ComPortConfig>,
    /// Secondary UART (COM2).
    #[serde(default)]
    pub com2: Option<ComPortConfig>,
    /// PS/2 keyboard controller.
    #[serde(default)]
    pub keyboard: Option<KbcConfig>,
    /// PS/2 mouse.
    #[serde(default)]
    pub mouse: Option<MouseConfig>,
    /// Embedded / environment controller.
    #[serde(default)]
    pub env_controller: Option<EcConfig>,
    /// Parallel port.
    #[serde(default)]
    pub parallel: Option<ParallelConfig>,
    /// Consumer IR receiver.
    #[serde(default)]
    pub cir: Option<CirConfig>,
    /// General-purpose I/O.
    #[serde(default)]
    pub gpio: Option<GpioConfig>,
    /// ACPI namespace name for the SuperIO container device (e.g., "SIO0").
    ///
    /// When set, ACPI generation emits child device nodes for each
    /// enabled LDN (COM1, KBC, mouse, etc.) with standard PNP HIDs.
    #[serde(default)]
    pub acpi_name: Option<heapless::String<8>>,
}

/// 16550-compatible UART settings exposed by the SuperIO.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ComPortConfig {
    /// I/O base port (e.g., `0x3F8`).
    pub io_base: u16,
    /// IRQ number.
    pub irq: u8,
    /// Desired baud rate (default 115200).
    #[serde(default = "default_baud")]
    pub baud_rate: u32,
}

fn default_baud() -> u32 {
    115200
}

/// PS/2 keyboard controller settings.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct KbcConfig {
    /// Primary I/O base (typically `0x60`).
    pub io_base: u16,
    /// Extended I/O base (typically `0x64`).
    pub io_ext: u16,
    /// IRQ number (typically 1).
    pub irq: u8,
}

/// PS/2 mouse settings.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct MouseConfig {
    /// IRQ number (typically 12).
    pub irq: u8,
}

/// Embedded controller / environment controller settings.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct EcConfig {
    /// Primary EC I/O base.
    pub io_base: u16,
    /// Secondary (extended) EC I/O base.
    pub io_ext: u16,
}

/// Parallel port settings.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ParallelConfig {
    /// I/O base port (typically `0x378`).
    pub io_base: u16,
    /// IRQ number (typically 7).
    pub irq: u8,
}

/// Consumer IR receiver settings.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct CirConfig {
    /// I/O base port.
    pub io_base: u16,
    /// IRQ number.
    pub irq: u8,
}

/// GPIO block settings.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct GpioConfig {
    /// Base I/O port for the GPIO registers.
    pub io_base: u16,
}

// ---------------------------------------------------------------------------
// Generic SuperIo<C> driver
// ---------------------------------------------------------------------------

/// Generic SuperIO driver parameterized by a [`SuperIoChip`] descriptor.
///
/// Constructed via [`BusDevice::new_on_bus`] — the base port comes from
/// the parent LPC bus, not from the config. Board authors set
/// `bus: Lpc(0x2e)` in the RON and the LPC bus driver passes the port
/// number through when constructing the child.
///
/// When the `com1` config is present, the driver implements [`Console`]
/// by accessing the classic NS16550 registers at `com1.io_base`. This
/// makes the SuperIO usable as an early console without needing a
/// separate NS16550 device in the RON.
pub struct SuperIo<C: SuperIoChip> {
    /// LPC config index port (e.g., `0x2e` or `0x4e`).
    base_port: u16,
    /// Saved config (used at `init()` time to actually program the chip).
    config: SuperIoConfig,
    _phantom: PhantomData<C>,
}

// SAFETY: All I/O is port-based with CPU-exclusive ownership; no shared
// state across threads (firmware is single-threaded).
unsafe impl<C: SuperIoChip> Send for SuperIo<C> {}
unsafe impl<C: SuperIoChip> Sync for SuperIo<C> {}

impl<C: SuperIoChip> SuperIo<C> {
    /// Index port (writes select the config register).
    #[inline]
    fn idx_port(&self) -> u16 {
        self.base_port
    }

    /// Data port (reads/writes the currently-selected register).
    #[inline]
    fn data_port(&self) -> u16 {
        self.base_port + 1
    }

    /// Enter configuration mode by writing the chip-specific sequence.
    fn enter_config(&self) {
        for b in C::ENTER_SEQ {
            // SAFETY: base_port is from the board RON, `b` is a chip constant.
            unsafe { fstart_pio::outb(self.idx_port(), *b) };
        }
        // Some chips (ITE) need a port-dependent final byte.
        if let Some(last) = C::enter_last_byte(self.base_port) {
            unsafe { fstart_pio::outb(self.idx_port(), last) };
        }
    }

    /// Exit configuration mode.
    fn exit_config(&self) {
        // SAFETY: chip-provided constants; base_port validated in new_on_bus.
        unsafe {
            fstart_pio::outb(self.idx_port(), C::EXIT_REG);
            fstart_pio::outb(self.data_port(), C::EXIT_VAL);
        }
    }

    /// Write an 8-bit value to a config register at `reg`.
    fn write_reg(&self, reg: u8, val: u8) {
        // SAFETY: callers always bracket writes with enter_config/exit_config.
        unsafe {
            fstart_pio::outb(self.idx_port(), reg);
            fstart_pio::outb(self.data_port(), val);
        }
    }

    /// Read an 8-bit value from a config register at `reg`.
    fn read_reg(&self, reg: u8) -> u8 {
        // SAFETY: callers always bracket reads with enter_config/exit_config.
        unsafe {
            fstart_pio::outb(self.idx_port(), reg);
            fstart_pio::inb(self.data_port())
        }
    }

    /// Select the given Logical Device Number (LDN 0x07 register).
    fn select_ldn(&self, ldn: u8) {
        self.write_reg(0x07, ldn);
    }

    /// Read the 16-bit chip ID from config registers `0x20`/`0x21`.
    fn read_chip_id(&self) -> u16 {
        let hi = self.read_reg(0x20) as u16;
        let lo = self.read_reg(0x21) as u16;
        (hi << 8) | lo
    }

    /// Program a COM-port LDN (io_base, IRQ, enable).
    fn program_com(&self, ldn: u8, cfg: &ComPortConfig) {
        self.select_ldn(ldn);
        // 0x60/0x61 = I/O base (high/low)
        self.write_reg(0x60, (cfg.io_base >> 8) as u8);
        self.write_reg(0x61, (cfg.io_base & 0xFF) as u8);
        // 0x70 = IRQ
        self.write_reg(0x70, cfg.irq);
        // 0x30 = enable (bit 0)
        self.write_reg(0x30, 0x01);
    }

    /// Program an EC/env-controller LDN (two I/O bases).
    fn program_ec(&self, ldn: u8, cfg: &EcConfig) {
        self.select_ldn(ldn);
        self.write_reg(0x60, (cfg.io_base >> 8) as u8);
        self.write_reg(0x61, (cfg.io_base & 0xFF) as u8);
        self.write_reg(0x62, (cfg.io_ext >> 8) as u8);
        self.write_reg(0x63, (cfg.io_ext & 0xFF) as u8);
        self.write_reg(0x30, 0x01);
    }

    /// Program the keyboard controller LDN.
    fn program_kbc(&self, ldn: u8, cfg: &KbcConfig) {
        self.select_ldn(ldn);
        self.write_reg(0x60, (cfg.io_base >> 8) as u8);
        self.write_reg(0x61, (cfg.io_base & 0xFF) as u8);
        self.write_reg(0x62, (cfg.io_ext >> 8) as u8);
        self.write_reg(0x63, (cfg.io_ext & 0xFF) as u8);
        self.write_reg(0x70, cfg.irq);
        self.write_reg(0x30, 0x01);
    }

    /// Program the mouse LDN (IRQ only).
    fn program_mouse(&self, ldn: u8, cfg: &MouseConfig) {
        self.select_ldn(ldn);
        self.write_reg(0x70, cfg.irq);
        self.write_reg(0x30, 0x01);
    }

    /// Program a simple single-base LDN (parallel, CIR).
    fn program_simple(&self, ldn: u8, io_base: u16, irq: u8) {
        self.select_ldn(ldn);
        self.write_reg(0x60, (io_base >> 8) as u8);
        self.write_reg(0x61, (io_base & 0xFF) as u8);
        self.write_reg(0x70, irq);
        self.write_reg(0x30, 0x01);
    }

    /// Program the GPIO LDN (io_base, no IRQ).
    fn program_gpio(&self, ldn: u8, cfg: &GpioConfig) {
        self.select_ldn(ldn);
        self.write_reg(0x62, (cfg.io_base >> 8) as u8);
        self.write_reg(0x63, (cfg.io_base & 0xFF) as u8);
        self.write_reg(0x30, 0x01);
    }
}

// ---------------------------------------------------------------------------
// BusDevice impl — constructed by the parent LPC bus
// ---------------------------------------------------------------------------

/// The parent bus type for SuperIO. Any driver providing an LPC bus
/// service with a `base_port(...)` getter can serve as the parent.
///
/// For the initial implementation we use `()` as the bus type and
/// require the caller to pass the base port in a different way; a
/// future revision will tie this to the `LpcBus` trait once its
/// concrete shape is settled. For now, `new_on_bus` takes the base
/// port directly encoded in the parent reference.
pub trait LpcBaseProvider {
    /// Return the LPC config index port for this child.
    fn lpc_base(&self) -> u16;
}

impl<C: SuperIoChip> BusDevice for SuperIo<C> {
    const NAME: &'static str = "superio";
    const COMPATIBLE: &'static [&'static str] = &[];
    type Config = SuperIoConfig;
    type Bus = dyn LpcBaseProvider;

    fn new_on_bus(config: &Self::Config, bus: &Self::Bus) -> Result<Self, DeviceError> {
        let base_port = bus.lpc_base();
        if base_port == 0 {
            return Err(DeviceError::MissingResource("lpc_base"));
        }
        Ok(Self {
            base_port,
            config: config.clone(),
            _phantom: PhantomData,
        })
    }

    fn init(&mut self) -> Result<(), DeviceError> {
        self.enter_config();

        // Chip ID sanity check.
        let id = self.read_chip_id();
        if id != C::CHIP_ID {
            self.exit_config();
            fstart_log::error!("superio: chip ID mismatch: read {:#06x}", id);
            return Err(DeviceError::InitFailed);
        }

        C::chip_init(self.base_port);

        // Program each enabled function. If the chip doesn't support
        // an LDN, we skip silently — the config for that field is ignored.
        if let (Some(ldn), Some(cfg)) = (C::COM1_LDN, self.config.com1) {
            self.program_com(ldn, &cfg);
        }
        if let (Some(ldn), Some(cfg)) = (C::COM2_LDN, self.config.com2) {
            self.program_com(ldn, &cfg);
        }
        if let (Some(ldn), Some(cfg)) = (C::KBC_LDN, self.config.keyboard) {
            self.program_kbc(ldn, &cfg);
        }
        if let (Some(ldn), Some(cfg)) = (C::MOUSE_LDN, self.config.mouse) {
            self.program_mouse(ldn, &cfg);
        }
        if let (Some(ldn), Some(cfg)) = (C::EC_LDN, self.config.env_controller) {
            self.program_ec(ldn, &cfg);
        }
        if let (Some(ldn), Some(cfg)) = (C::PARALLEL_LDN, self.config.parallel) {
            self.program_simple(ldn, cfg.io_base, cfg.irq);
        }
        if let (Some(ldn), Some(cfg)) = (C::CIR_LDN, self.config.cir) {
            self.program_simple(ldn, cfg.io_base, cfg.irq);
        }
        if let (Some(ldn), Some(cfg)) = (C::GPIO_LDN, self.config.gpio) {
            self.program_gpio(ldn, &cfg);
        }

        self.exit_config();
        Ok(())
    }
}

// The SuperIO driver intentionally does NOT implement `Console`.
//
// A SuperIO chip's job is purely logical-device programming: it enters
// configuration mode, selects each enabled LDN via register 0x07, and
// writes the I/O base / IRQ / enable pair. Once those are set, each
// LDN appears at its programmed I/O address as a regular peripheral
// (NS16550-compatible UART for COM ports, 8042 for KBC, etc.).
//
// Actual UART I/O is handled by a separate NS16550 driver declared as
// a *plain-Device* child of the SuperIO in the board RON. Declaring
// COM1 and COM2 as two independent children lets the board author
// pick either — or both — as the `Console` provider without hardcoding
// the choice here.
//
// The init-ordering guarantee is provided by codegen: a ConsoleInit
// referencing `com1` emits `southbridge.init()` → `superio.init()` →
// `com1.init()` via `ensure_device_ready`, so LPC decode is open and
// the SuperIO LDN is programmed before the NS16550 code touches the
// UART registers.

// ---------------------------------------------------------------------------
// ACPI device generation — one DSDT node per enabled logical device
// ---------------------------------------------------------------------------

#[cfg(feature = "acpi")]
mod acpi_impl {
    extern crate alloc;
    use alloc::vec::Vec;
    use fstart_acpi::device::AcpiDevice;

    use super::*;

    // PNP ACPI HIDs for standard SuperIO logical devices.
    const HID_COM: &str = "PNP0501"; // 16550A-compatible COM port
    const HID_KBC: &str = "PNP0303"; // IBM enhanced keyboard (101/102-key)
    const HID_MOUSE: &str = "PNP0F13"; // PS/2 port for PS/2-style mice
    const HID_LPT: &str = "PNP0400"; // Standard LPT parallel port
    const HID_EC: &str = "PNP0C09"; // Embedded controller

    /// Emit an IO resource descriptor for the given port/size, plus an
    /// optional IRQ descriptor.
    fn ldn_device(
        name: &str,
        hid: &str,
        uid: u32,
        io_base: u16,
        io_size: u16,
        irq: Option<u8>,
    ) -> Vec<u8> {
        let gsiv = irq.unwrap_or(0) as u32;
        if let Some(_irq) = irq {
            fstart_acpi_macros::acpi_dsl! {
                Device(#{name}) {
                    Name("_HID", EisaId(#{hid}));
                    Name("_UID", #{uid});
                    Name("_CRS", ResourceTemplate {
                        IO(Decode16, #{io_base}, #{io_base}, 0x01u8, #{io_size});
                        IRQ(Edge, ActiveHigh, Exclusive, #{gsiv});
                    });
                }
            }
        } else {
            fstart_acpi_macros::acpi_dsl! {
                Device(#{name}) {
                    Name("_HID", EisaId(#{hid}));
                    Name("_UID", #{uid});
                    Name("_CRS", ResourceTemplate {
                        IO(Decode16, #{io_base}, #{io_base}, 0x01u8, #{io_size});
                    });
                }
            }
        }
    }

    /// KBC needs two I/O ranges (0x60 and 0x64).
    fn kbc_device(cfg: &KbcConfig) -> Vec<u8> {
        let base1 = cfg.io_base;
        let base2 = cfg.io_ext;
        let gsiv = cfg.irq as u32;
        fstart_acpi_macros::acpi_dsl! {
            Device("KBD0") {
                Name("_HID", EisaId("PNP0303"));
                Name("_UID", 0u32);
                Name("_CRS", ResourceTemplate {
                    IO(Decode16, #{base1}, #{base1}, 0x01u8, 0x01u8);
                    IO(Decode16, #{base2}, #{base2}, 0x01u8, 0x01u8);
                    IRQ(Edge, ActiveHigh, Exclusive, #{gsiv});
                });
            }
        }
    }

    /// Mouse shares KBC ports but has its own IRQ.
    fn mouse_device(kbc: &KbcConfig, mouse: &MouseConfig) -> Vec<u8> {
        let base1 = kbc.io_base;
        let base2 = kbc.io_ext;
        let gsiv = mouse.irq as u32;
        fstart_acpi_macros::acpi_dsl! {
            Device("MOU0") {
                Name("_HID", EisaId("PNP0F13"));
                Name("_UID", 0u32);
                Name("_CRS", ResourceTemplate {
                    IO(Decode16, #{base1}, #{base1}, 0x01u8, 0x01u8);
                    IO(Decode16, #{base2}, #{base2}, 0x01u8, 0x01u8);
                    IRQ(Edge, ActiveHigh, Exclusive, #{gsiv});
                });
            }
        }
    }

    /// EC needs two I/O ranges.
    fn ec_device(cfg: &EcConfig) -> Vec<u8> {
        let base1 = cfg.io_base;
        let base2 = cfg.io_ext;
        fstart_acpi_macros::acpi_dsl! {
            Device("EC00") {
                Name("_HID", EisaId("PNP0C09"));
                Name("_UID", 0u32);
                Name("_CRS", ResourceTemplate {
                    IO(Decode16, #{base1}, #{base1}, 0x01u8, 0x08u8);
                    IO(Decode16, #{base2}, #{base2}, 0x01u8, 0x08u8);
                });
            }
        }
    }

    impl<C: SuperIoChip> AcpiDevice for SuperIo<C> {
        type Config = SuperIoConfig;

        /// Produce DSDT AML for all enabled SuperIO logical devices.
        ///
        /// Each enabled function gets its own Device node with the
        /// appropriate PNP HID, `_UID`, and `_CRS` (IO + IRQ resources).
        /// Nodes are nested inside the parent LPC bridge scope by the
        /// ACPI assembler.
        fn dsdt_aml(&self, config: &Self::Config) -> Vec<u8> {
            let mut aml = Vec::new();

            // COM1
            if let Some(ref com) = config.com1 {
                aml.extend(ldn_device(
                    "COM1",
                    HID_COM,
                    0,
                    com.io_base,
                    8,
                    Some(com.irq),
                ));
            }

            // COM2
            if let Some(ref com) = config.com2 {
                aml.extend(ldn_device(
                    "COM2",
                    HID_COM,
                    1,
                    com.io_base,
                    8,
                    Some(com.irq),
                ));
            }

            // PS/2 Keyboard
            if let Some(ref kbc) = config.keyboard {
                aml.extend(kbc_device(kbc));
            }

            // PS/2 Mouse (needs KBC ports for shared I/O)
            if let (Some(ref kbc), Some(ref mouse)) = (&config.keyboard, &config.mouse) {
                aml.extend(mouse_device(kbc, mouse));
            }

            // Parallel port
            if let Some(ref pp) = config.parallel {
                aml.extend(ldn_device("LPT0", HID_LPT, 0, pp.io_base, 8, Some(pp.irq)));
            }

            // Environment controller
            if let Some(ref ec) = config.env_controller {
                aml.extend(ec_device(ec));
            }

            // Consumer IR
            if let Some(ref cir) = config.cir {
                aml.extend(ldn_device(
                    "CIR0",
                    "PNP0510",
                    0,
                    cir.io_base,
                    8,
                    Some(cir.irq),
                ));
            }

            aml
        }
    }
}
