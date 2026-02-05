# fstart Driver Model

## Status

Design document. Implementation tracked in phases at the end of this file.

## Goals

1. **Fully type-safe at compile time** -- no `void *`, no runtime downcasts, no
   linker-section magic.
2. **RON-driven** -- the board `.ron` file remains the single source of truth; codegen
   produces all glue code.
3. **Zero-cost abstractions** -- in `Rigid` mode every call is monomorphized; no trait
   objects, no vtables.
4. **`no_std` / no-alloc** -- bounded containers, static lifetimes, no heap.
5. **Layered** -- clean separation between *service interfaces*, *device classes*,
   *drivers*, and *board wiring*.

## Prior Art

The design draws from two mature C firmware frameworks and improves on them using
Rust's type system.

### coreboot (~/src/coreboot)

coreboot's model centres on `struct device` nodes arranged in a tree compiled from
`devicetree.cb`.  Each node carries a `device_operations` vtable (flat, ~15 function
pointers covering PCI, ACPI, GPIO, and init) plus a `chip_operations` for
registration.  Drivers are bound via a naming convention
(`drivers/i2c/generic` -> `drivers_i2c_generic_ops`).

**Strengths adopted:** compile-time device tree, resource model, override trees.

**Weaknesses avoided:** no service/interface concept (everything is a flat vtable),
`void *chip_info`, no type safety on config data, impoverished early-stage access,
global mutable state.

### U-Boot DM (~/src/u-boot)

U-Boot's Driver Model introduces *uclasses* -- interface categories (serial, I2C, MMC)
with typed ops structs (`struct dm_serial_ops`).  Devices are discovered from a
flattened device tree via compatible-string matching.  Each `struct udevice` holds
`void *plat_` (config from DT), `void *priv_` (runtime state), and `void *ops`.
Probing is lazy: devices are *bound* (allocated) at scan time but only *probed*
(hardware-initialised) on first use.

**Strengths adopted:** uclass/ops separation, lazy init, separate plat vs priv,
bus-hierarchy model, compatible-string matching.

**Weaknesses avoided:** `void *` ops (no compile-time check that a driver's ops
matches its uclass), linker-list registration fragility, linear driver search, no
compile-time device-tree validation.

## Architecture Overview

```
+----------------------------------------------------------------+
|                        Board RON File                           |
|  (devices, buses, resources, capabilities, topology)           |
+----------------------------+-----------------------------------+
                             | codegen (build.rs)
                             v
+----------------------------------------------------------------+
|                    Generated Stage Code                         |
|  * Concrete type aliases (type BoardConsole = Ns16550)         |
|  * Static device instances                                     |
|  * Devices struct (typed, one field per device)                |
|  * StageContext (typed service accessors)                      |
|  * fstart_main() init sequence from capabilities               |
+----------+-----------------+-------------------+---------------+
           |                 |                   |
           v                 v                   v
  +---------------+  +---------------+  +--------------------+
  | fstart-       |  | fstart-       |  | fstart-            |
  | services      |  | drivers       |  | capabilities       |
  |               |  |               |  |                    |
  | trait Console |  | Ns16550       |  | console_init()     |
  | trait Timer   |  | Pl011         |  | memory_init()      |
  | trait Block   |  | DesignwareI2c |  | sig_verify()       |
  | trait I2cBus  |  |               |  |                    |
  | trait Device  |  | impl Device   |  | StageContext       |
  +---------------+  +---------------+  +--------------------+
```

## Layer 1 -- Service Traits (fstart-services)

Service traits define **what a category of hardware can do**.  They are the Rust
equivalent of U-Boot's uclass ops structs, but with full compile-time enforcement.

Existing traits are kept as-is:

```rust
pub trait Console: Send + Sync {
    fn write_byte(&self, byte: u8) -> Result<(), ServiceError>;
    fn read_byte(&self) -> Result<Option<u8>, ServiceError>;
    // ... default methods
}

pub trait BlockDevice: Send + Sync {
    fn read(&self, offset: u64, buf: &mut [u8]) -> Result<usize, ServiceError>;
    fn write(&self, offset: u64, buf: &[u8]) -> Result<usize, ServiceError>;
    fn size(&self) -> u64;
    // ... default methods
}

pub trait Timer: Send + Sync {
    fn delay_us(&self, us: u64);
    fn timestamp_us(&self) -> u64;
    // ... default methods
}
```

New bus-level service traits are added:

```rust
/// A controller that can perform I2C transactions to child addresses.
pub trait I2cBus: Send + Sync {
    fn read(&self, addr: u8, reg: u8, buf: &mut [u8]) -> Result<usize, ServiceError>;
    fn write(&self, addr: u8, reg: u8, data: &[u8]) -> Result<usize, ServiceError>;
}

/// A controller that can perform SPI transactions via chip-select lines.
pub trait SpiBus: Send + Sync {
    fn transfer(&self, cs: u8, tx: &[u8], rx: &mut [u8]) -> Result<usize, ServiceError>;
}

/// A controller that manages GPIO pins.
pub trait GpioController: Send + Sync {
    fn get(&self, pin: u32) -> Result<bool, ServiceError>;
    fn set(&self, pin: u32, value: bool) -> Result<(), ServiceError>;
    fn set_direction(&self, pin: u32, output: bool) -> Result<(), ServiceError>;
}
```

### Why traits instead of ops structs?

In coreboot, `device_operations` is a single flat vtable mixing PCI BARs, ACPI
generation, GPIO, and init lifecycle.  In U-Boot, ops structs are typed per uclass
but assigned via `const void *` -- a driver can accidentally point `.ops` at the
wrong struct type.  In Rust, `impl Console for Ns16550` is verified at compile time:
every required method must exist with the exact signature.

## Layer 2 -- The Device Trait (fstart-services)

Every hardware device implements the `Device` trait, which captures the lifecycle
that coreboot splits across `chip_operations` + `device_operations` and that U-Boot
splits across `bind` + `of_to_plat` + `probe`:

```rust
/// Base trait for all hardware devices.
pub trait Device: Send + Sync + Sized {
    /// Human-readable driver name (e.g., "ns16550").
    const NAME: &'static str;

    /// Compatible strings for matching (e.g., &["ns16550a", "ns16550"]).
    const COMPATIBLE: &'static [&'static str];

    /// Driver-specific configuration type.  Replaces the flat `Resources`
    /// struct with a per-driver typed config (like U-Boot's plat).
    type Config;

    /// Construct from typed config.  Equivalent to U-Boot's bind + of_to_plat.
    /// Does NOT touch hardware -- only stores configuration.
    fn new(config: &Self::Config) -> Result<Self, DeviceError>;

    /// Initialise hardware.  Equivalent to U-Boot's probe().
    /// Separated from new() so codegen can control init ordering.
    fn init(&self) -> Result<(), DeviceError>;
}
```

### Associated `type Config`

The current flat `Resources` struct is a grab-bag of optional fields; every driver
ignores most of them.  With an associated type, each driver declares exactly what it
needs:

```rust
// In fstart-drivers:
pub struct Ns16550Config {
    pub base_addr: u64,
    pub clock_freq: u32,
    pub baud_rate: u32,
}

impl Device for Ns16550 {
    const NAME: &'static str = "ns16550";
    const COMPATIBLE: &'static [&'static str] = &["ns16550a", "ns16550"];
    type Config = Ns16550Config;

    fn new(config: &Ns16550Config) -> Result<Self, DeviceError> {
        Ok(Self { regs: unsafe { &*(config.base_addr as *const Ns16550Regs) } })
    }

    fn init(&self) -> Result<(), DeviceError> {
        // FIFO enable, 8N1, divisor latch...
        Ok(())
    }
}
```

Codegen maps the RON `Resources` fields to the concrete config type at build time:

```rust
// Generated:
let uart0 = Ns16550::new(&Ns16550Config {
    base_addr: 0x1000_0000,
    clock_freq: 3_686_400,
    baud_rate: 115_200,
}).unwrap_or_else(|_| halt_with_error());
```

A missing required field in the RON is caught at build time by codegen (which emits
`compile_error!`), not at runtime.

### Bus Devices

Devices that live on a bus declare their parent bus requirement:

```rust
/// Marker: this device lives on a parent bus.
pub trait BusDevice: Device {
    /// The service trait this device requires from its parent.
    type ParentBus;
}
```

Example: an I2C-attached TPM:

```rust
impl Device for Slb9670 {
    type Config = Slb9670Config;
    // ...
}

impl BusDevice for Slb9670 {
    type ParentBus = dyn I2cBus;
}
```

## Layer 3 -- Device Tree in RON (fstart-types)

### Hierarchical Device Declarations

Bus hierarchies are expressed via a `parent` field on child devices.  This avoids
recursive types (which would require heap allocation in a `no_std` context) while
keeping the device list flat and easy to serialise with `heapless::Vec`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceConfig {
    pub name: HString<32>,
    pub compatible: HString<64>,
    pub driver: HString<32>,
    pub services: heapless::Vec<HString<32>, 8>,
    pub resources: Resources,
    /// Parent device name (for bus-attached devices).
    /// `None` for root-level devices.
    #[serde(default)]
    pub parent: Option<HString<32>>,
}
```

Board RON example with a bus:

```ron
devices: [
    (
        name: "uart0",
        compatible: "ns16550a",
        driver: "ns16550",
        services: ["Console"],
        resources: ( mmio_base: Some(0x10000000), clock_freq: Some(3686400),
                     baud_rate: Some(115200), irq: Some(10) ),
    ),
    (
        name: "i2c0",
        compatible: "dw-apb-i2c",
        driver: "designware-i2c",
        services: ["I2cBus"],
        resources: ( mmio_base: Some(0x10040000), clock_freq: Some(100000000) ),
    ),
    (
        name: "tpm0",
        compatible: "infineon,slb9670",
        driver: "slb9670",
        services: ["Tpm"],
        resources: ( bus_addr: Some(0x50) ),
        parent: Some("i2c0"),
    ),
]
```

Codegen sorts devices topologically (parents before children) and validates that
every `parent` reference names an existing device that provides a bus service.

The `Resources` struct remains as the **RON interchange format**.  It is deliberately
flat and permissive (all fields `Option`).  Codegen is responsible for validating that
each driver's required resources are present and mapping them to the driver's typed
`Config`.

## Layer 4 -- Generated Code (fstart-codegen)

This is where fstart diverges most from coreboot and U-Boot.  Instead of runtime
data structures populated from a device tree, codegen produces **concrete Rust types
at build time**.

### Devices Struct

One field per device, using the exact driver type:

```rust
// Generated for qemu-riscv64:
struct Devices {
    uart0: fstart_drivers::uart::ns16550::Ns16550,
}
```

For boards with bus hierarchies:

```rust
struct Devices {
    uart0: Ns16550,
    i2c0: DesignwareI2c,
    tpm0: Slb9670,
}
```

### StageContext

Provides typed access to services.  No trait objects in Rigid mode:

```rust
struct StageContext {
    devices: Devices,
}

impl StageContext {
    #[inline]
    fn console(&self) -> &impl Console {
        &self.devices.uart0
    }
}
```

### Init Sequence

Generated from the capability list in RON.  The order in the RON file IS the init
order:

```rust
#[no_mangle]
pub extern "Rust" fn fstart_main() -> ! {
    // --- Device construction (bind phase) ---
    let uart0 = Ns16550::new(&Ns16550Config {
        base_addr: 0x1000_0000,
        clock_freq: 3_686_400,
        baud_rate: 115_200,
    }).unwrap_or_else(|_| fstart_platform_riscv64::halt());

    // --- Capability: ConsoleInit(device: "uart0") ---
    uart0.init().unwrap_or_else(|_| fstart_platform_riscv64::halt());
    let _ = uart0.write_line("[fstart] uart0: ns16550 console ready");

    // --- Build context ---
    let ctx = StageContext { devices: Devices { uart0 } };

    // --- Capability: MemoryInit ---
    fstart_capabilities::memory_init(ctx.console());

    // --- Capability: PayloadLoad ---
    // fstart_capabilities::payload_load(ctx.console(), ctx.block());

    let _ = ctx.console().write_line("[fstart] all capabilities complete");
    fstart_platform_riscv64::halt()
}
```

### Bus Ordering

For bus hierarchies, codegen enforces **parent before child** init ordering (matching
both coreboot's tree-walk and U-Boot's recursive-probe pattern):

```rust
// Parent first:
let i2c0 = DesignwareI2c::new(&DesignwareI2cConfig { ... })?;
i2c0.init()?;

// Then child, receiving a reference to the parent bus:
let tpm0 = Slb9670::new(&Slb9670Config { bus: &i2c0, addr: 0x50 })?;
tpm0.init()?;
```

### Codegen Validation

The codegen phase performs these checks at `build.rs` time, emitting
`compile_error!()` in the generated source for any failure:

| Check | Error |
|-------|-------|
| Capability references unknown device name | `"ConsoleInit references device 'foo' which is not declared"` |
| Device declares unknown driver | `"Device 'uart0' uses driver 'ns16999' which is not known"` |
| Driver's required resources missing from RON | `"Driver 'ns16550' requires mmio_base but device 'uart0' does not provide it"` |
| Capability uses device that doesn't provide required service | `"ConsoleInit requires Console service but device 'gpio0' does not provide it"` |
| Child device's `parent` names a device without a bus service | `"Device 'tpm0' has parent 'gpio0' which does not provide a bus service"` |
| Child device's `parent` names a non-existent device | `"Device 'tpm0' has parent 'nonexistent' which is not declared"` |

## Layer 5 -- Rigid vs Flexible Mode

The `mode` field in the board RON controls how dispatch works.

### Rigid (default)

All types are concrete.  Codegen emits direct struct fields, `&impl Trait` returns,
and monomorphised function calls.  The compiler inlines everything and eliminates
unused code.  **Zero runtime overhead.**

```rust
// Rigid: concrete type, no indirection
fn console(&self) -> &impl Console { &self.devices.uart0 }
```

### Flexible

For boards that may select between drivers at runtime (e.g., detect which UART is
present), codegen produces an enum:

```rust
// Flexible: enum dispatch, no trait objects, no vtable
enum ConsoleDevice {
    Ns16550(Ns16550),
    Pl011(Pl011),
}

impl Console for ConsoleDevice {
    fn write_byte(&self, byte: u8) -> Result<(), ServiceError> {
        match self {
            Self::Ns16550(d) => d.write_byte(byte),
            Self::Pl011(d) => d.write_byte(byte),
        }
    }
    // ...
}
```

The enum variants are generated from the set of drivers that the board's devices
declare.  This avoids `dyn Trait` (no vtable allocation, no pointer indirection)
while still allowing runtime selection.  The match arms are exhaustive and
compiler-checked.

## Lifecycle Comparison

| Phase | coreboot | U-Boot | fstart |
|-------|----------|--------|--------|
| **Describe** | `devicetree.cb` (custom DSL, sconfig compiler) | FDT blob (dtc compiler) | `board.ron` (serde, `ron` crate) |
| **Bind** | `chip_ops->enable_dev()` assigns vtable | `device_bind_common()` allocates `udevice` | Codegen emits `Device::new()` call |
| **Configure** | `read_resources()` + allocator | `of_to_plat()` reads DT properties | Codegen maps RON `Resources` -> typed `Config` |
| **Probe/Init** | `ops->init()` in fixed tree order | `drv->probe()` lazily on first access | `Device::init()` in capability order |
| **Use** | Call `ops` function pointers | Cast `dev->driver->ops` to typed struct | Call trait methods (compiler-verified) |
| **Finalize** | `ops->final()` | `drv->remove()` | Platform `halt()` (firmware does not return) |

## Error Handling

| Context | Type | Notes |
|---------|------|-------|
| Device construction | `Result<Self, DeviceError>` | `DeviceError::MissingResource`, `ConfigError` |
| Device init | `Result<(), DeviceError>` | `DeviceError::InitFailed`, `HardwareError` |
| Service operations | `Result<T, ServiceError>` | `Timeout`, `HardwareError`, `IoError` |
| Codegen validation | `compile_error!("...")` | Build fails with clear message |
| Generated init code | `.unwrap_or_else(\|_\| halt())` | Explicit halt on init failure |

The `DeviceError` enum unifies the current `DriverError`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceError {
    /// A required resource was not provided in the board config.
    MissingResource(&'static str),
    /// The device configuration is invalid.
    ConfigError,
    /// Hardware did not respond as expected during init.
    InitFailed,
    /// A bus error occurred communicating with a parent bus.
    BusError,
}
```

## Driver Author Checklist

To add a new driver (e.g., a SiFive UART):

1. **Create the module**: `fstart-drivers/src/uart/sifive.rs`, feature-gated under
   `sifive-uart`.

2. **Define registers** with `register_structs!` / `register_bitfields!`:
   ```rust
   register_bitfields! [u32,
       TXDATA [ FULL OFFSET(31) NUMBITS(1) [], DATA OFFSET(0) NUMBITS(8) [] ],
       RXDATA [ EMPTY OFFSET(31) NUMBITS(1) [], DATA OFFSET(0) NUMBITS(8) [] ],
   ];
   ```

3. **Define the config type**:
   ```rust
   pub struct SifiveUartConfig {
       pub base_addr: u64,
       pub clock_freq: u32,
       pub baud_rate: u32,
   }
   ```

4. **Implement `Device`**:
   ```rust
   impl Device for SifiveUart {
       const NAME: &'static str = "sifive-uart";
       const COMPATIBLE: &'static [&'static str] = &["sifive,uart0"];
       type Config = SifiveUartConfig;
       fn new(config: &Self::Config) -> Result<Self, DeviceError> { ... }
       fn init(&self) -> Result<(), DeviceError> { ... }
   }
   ```

5. **Implement service traits**:
   ```rust
   impl Console for SifiveUart { ... }
   ```

6. **Register in codegen**: add the driver name to the match table in
   `fstart-codegen/src/stage_gen.rs` so codegen knows how to map the RON
   `driver: "sifive-uart"` to the concrete type and config mapping.

7. **Add feature flag**: in `fstart-drivers/Cargo.toml` and `fstart-stage/Cargo.toml`.

8. **Test**: write a board RON using the driver and verify with
   `cargo xtask build --board <name>`.

## Implementation Phases

### Phase 1: Foundation

- [ ] Add `Device` trait (with associated `type Config`) and `DeviceError` to
      `fstart-services`.
- [ ] Add `Ns16550Config`, `Pl011Config` structs to `fstart-drivers`.
- [ ] Implement `Device` for `Ns16550` and `Pl011`.
- [ ] Add `BusDevice` trait to `fstart-services`.
- [ ] Add `parent` field (with `#[serde(default)]`) to `DeviceConfig` in
      `fstart-types` for bus hierarchies.
- [ ] Remove old `Driver` trait from `fstart-drivers` (replaced by `Device`).

### Phase 2: Codegen Upgrade

- [ ] Update `stage_gen.rs` to generate a `Devices` struct with concrete typed fields.
- [ ] Update `stage_gen.rs` to generate a `StageContext` with service accessor methods.
- [ ] Generate typed `Config` construction (map RON `Resources` -> driver configs).
- [ ] Wire `fstart_capabilities::console_init()` into generated code.
- [ ] Add codegen validation: missing devices, unknown drivers, service mismatches ->
      `compile_error!`.

### Phase 3: Capability Pipeline

- [ ] Implement capability functions that accept service trait references.
- [ ] Generate full init pipeline from capability list.
- [ ] Add ordering validation in codegen (e.g., ConsoleInit must precede capabilities
      that log).

### Phase 4: Bus Support

- [ ] Add bus service traits (`I2cBus`, `SpiBus`, `GpioController`) to
      `fstart-services`.
- [ ] Implement codegen support for `children` -- parent-before-child init ordering.
- [ ] Implement first bus driver (e.g., DesignWare I2C).

### Phase 5: Flexible Mode

- [ ] Implement enum-dispatch codegen for `mode: Flexible`.
- [ ] Generate `ConsoleDevice` / `TimerDevice` / etc. enums from the board's driver set.
- [ ] Implement the service traits on the generated enums.
