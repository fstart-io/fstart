# fstart Driver Model

## Status

Design document with implementation notes.  Phases 1–4 are substantially
complete; Phase 5 (Flexible mode) was superseded by the stage-runtime /
codegen split.  The driver model is functional: boards build, run in
QEMU, and codegen produces a typed board adapter (`impl Board for
_BoardDevices`) consumed by a handwritten generic executor
(`fstart_stage_runtime::run_stage`).

## Goals

1. **Fully type-safe at compile time** -- no `void *`, no runtime downcasts, no
   linker-section magic.
2. **RON-driven** -- the board `.ron` file remains the single source of truth; codegen
   produces all glue code.
3. **Zero-cost abstractions** -- the `Board` trait is `Sized` and the executor is
   generic over `B: Board`, so every call is monomorphized; no trait objects,
   no vtables.
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
|  * _BoardDevices struct (Option<Driver> per device)            |
|  * impl Board for _BoardDevices (capability trampolines)      |
|  * static STAGE_PLAN: StagePlan (CapOp sequence)              |
|  * fstart_main() stub → run_stage(board, plan, handoff)       |
+----------+-----------------+-------------------+---------------+
           |                 |                   |
           v                 v                   v
  +---------------+  +---------------+  +--------------------+
  | fstart-       |  | fstart-       |  | fstart-stage-      |
  | services      |  | drivers       |  | runtime            |
  |               |  |               |  |                    |
  | trait Console |  | Ns16550       |  | trait Board        |
  | trait Timer   |  | Pl011         |  | run_stage<B>()     |
  | trait Block   |  | DesignwareI2c |  | StagePlan, CapOp   |
  | trait I2cBus  |  |               |  | DeviceMask         |
  | trait Device  |  | impl Device   |  |                    |
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

Devices that live on a parent bus implement `BusDevice` **instead of** `Device`.
`BusDevice` is a standalone trait (not a subtrait of `Device`) with its own
`new_on_bus` constructor that receives a reference to the parent bus controller.
Codegen resolves parent variable names at build time — no runtime device lookup.

```rust
/// Trait for devices that live on a parent bus.
pub trait BusDevice: Send + Sync + Sized {
    const NAME: &'static str;
    const COMPATIBLE: &'static [&'static str];

    /// Driver-specific configuration type (deserialized from RON).
    type Config;

    /// The parent bus interface type (e.g., `B` where `B: I2c`).
    type Bus: ?Sized;

    /// Construct from config + parent bus reference.  Does NOT touch hardware.
    fn new_on_bus(config: &Self::Config, bus: &Self::Bus) -> Result<Self, DeviceError>;

    /// Initialise hardware.  Called after `new_on_bus()`, in capability order.
    fn init(&self) -> Result<(), DeviceError>;
}
```

Example: an I2C-attached TPM:

```rust
impl<B: I2c> BusDevice for Slb9670<B> {
    type Bus = B;
    type Config = Slb9670Config;

    fn new_on_bus(config: &Slb9670Config, bus: &B) -> Result<Self, DeviceError> {
        Ok(Self { bus, addr: config.addr })
    }

    fn init(&self) -> Result<(), DeviceError> {
        // Probe TPM identity register...
        Ok(())
    }
}
```

Codegen generates:
```rust
let tpm0 = Slb9670::new_on_bus(&tpm0_config, &i2c0)
    .unwrap_or_else(|_| halt());
```

## Layer 3 -- Device Tree in RON (fstart-types + fstart-codegen)

### Typed Driver Configuration (DriverInstance enum)

Instead of a flat `Resources` bag-of-options with `compatible` string matching,
each driver defines its own typed config struct in `fstart-drivers`, and the RON
uses a `DriverInstance` enum for type-safe dispatch:

```rust
// In fstart-drivers (the enum):
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DriverInstance {
    Ns16550(Ns16550Config),
    Pl011(Pl011Config),
    DesignwareI2c(DesignwareI2cConfig),
    // ...
}
```

RON uses the variant name directly — **no `compatible` string matching**, no
`Resources` mapping.  Serde validates the config fields at parse time:

```ron
(
    name: "uart0",
    driver: Ns16550(( base_addr: 0x10000000, clock_freq: 3686400, baud_rate: 115200 )),
    services: ["Console"],
)
```

Note the double-paren syntax: outer `()` = RON enum variant, inner `()` = anonymous
struct fields.

### Hierarchical Device Declarations (nested children)

Bus hierarchies are expressed via nested `children` in the RON file — the tree
structure is **structural**, not string-reference-based.  No `parent` field is
needed in the RON; hierarchy is implicit from nesting:

```ron
devices: [
    (
        name: "uart0",
        driver: Ns16550(( base_addr: 0x10000000, clock_freq: 3686400, baud_rate: 115200 )),
        services: ["Console"],
    ),
    (
        name: "i2c0",
        driver: DesignwareI2c(( base_addr: 0x10040000, clock_freq: 100000000 )),
        services: ["I2cBus"],
        children: [
            (
                name: "tpm0",
                driver: Slb9670(( addr: 0x50 )),
                services: ["Tpm"],
            ),
        ],
    ),
]
```

The `children` field defaults to `[]` via `#[serde(default)]`, so existing board
RON files with no bus hierarchies need no changes.

### RON → Flat Device Table

The codegen RON loader uses a recursive type `RonDevice` with nested children
(this is host-side `std` code, so `Vec` is fine).  During loading, `flatten_device()`
performs a pre-order DFS traversal that produces three parallel arrays:

1. **`devices: Vec<DeviceConfig>`** — flat list with `parent: Option<HString<32>>`
   (filled in from the tree structure during flattening)
2. **`driver_instances: Vec<DriverInstance>`** — typed config for each device
3. **`device_tree: Vec<DeviceNode>`** — flat index-based tree for the target binary

Pre-order DFS guarantees parents always precede children — no topological sort
needed (cycles are structurally impossible with nested children).

### DeviceNode — Runtime Device Tree (fstart-types)

The `DeviceNode` type is a cache-friendly, `no_std`-compatible, const-constructible
node for runtime introspection:

```rust
pub type DeviceId = u8;  // Max 256 devices per board

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceNode {
    /// Parent device index, or `None` for root devices.
    pub parent: Option<DeviceId>,
    /// Depth in the tree (0 = root, 1 = child of root, …).
    pub depth: u8,
}
```

Codegen emits a static table into the firmware binary:

```rust
// Generated:
static DEVICE_TREE: [fstart_types::DeviceNode; 3] = [
    fstart_types::DeviceNode { parent: None, depth: 0 },          // uart0
    fstart_types::DeviceNode { parent: None, depth: 0 },          // i2c0
    fstart_types::DeviceNode { parent: Some(1), depth: 1 },       // tpm0
];
```

No pointers, no linked lists — just indices into a flat array.  This is
approach B (flat index table) for runtime power sequencing, diagnostics, etc.

### DeviceConfig (host-side metadata)

The `DeviceConfig` struct carries identity and service bindings.  It is used
only at build time (codegen, xtask) — the target binary never sees it:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceConfig {
    pub name: HString<32>,
    pub driver: HString<32>,
    pub services: heapless::Vec<HString<32>, 8>,
    #[serde(default)]
    pub parent: Option<HString<32>>,  // Filled by flatten_device()
}
```

## Layer 4 -- Generated Code (fstart-codegen)

This is where fstart diverges most from coreboot and U-Boot.  Instead of runtime
data structures populated from a device tree, codegen produces **concrete Rust types
at build time**.

### _BoardDevices Struct

One `Option<Driver>` field per enabled device, plus bookkeeping state:

```rust
// Generated for qemu-riscv64:
struct _BoardDevices {
    uart0: Option<Ns16550>,
    // Bookkeeping (populated by new(), updated by trampolines):
    _inited: fstart_stage_runtime::DeviceMask,
    _boot_media: fstart_stage_runtime::BootMediaState,
    _dtb_dst_addr: u64,
    _bootargs: &'static str,
    _dram_base: u64,
    _dram_size_static: u64,
    _handoff: Option<fstart_types::handoff::StageHandoff>,
    _acpi_rsdp_addr: u64,
    _egon_sram_base: u64,
}
```

All device fields are `Option<T>` because `init_device(id)` is the sole
construction site — devices are lazily materialised when the executor asks
for them.

### impl Board for _BoardDevices

The codegen-emitted adapter implements the `Board` trait from
`fstart-stage-runtime`.  Each method dispatches to concrete driver fields
via a `match id { ... }` on `DeviceId`:

```rust
impl fstart_stage_runtime::Board for _BoardDevices {
    fn init_device(&mut self, id: DeviceId) -> Result<(), DeviceError> {
        match id {
            0 => {
                if self._inited.contains(0) { return Ok(()); }
                let cfg = Ns16550Config { base_addr: 0x1000_0000, clock: 0, baud: 115200 };
                let mut _dev = Ns16550::new(&cfg)?;
                _dev.init()?;
                self.uart0 = Some(_dev);
                self._inited.set(0);
                Ok(())
            }
            _ => Err(DeviceError::InitFailed),
        }
    }

    unsafe fn install_logger(&self, id: DeviceId) {
        match id {
            0 => {
                fstart_log::init(self.uart0.as_ref().unwrap_or_else(|| halt()));
                fstart_capabilities::console_ready("uart0", "ns16550");
            }
            _ => fstart_platform::halt(),
        }
    }

    fn sig_verify(&self) { /* reads &FSTART_ANCHOR + self._boot_media */ }
    fn fdt_prepare(&self) { /* reads self._dtb_dst_addr, self._bootargs, etc. */ }
    fn payload_load(&self) -> ! { /* loads kernel from FFS, jumps via platform */ }
    // ... 15 more methods
    fn halt(&self) -> ! { fstart_platform::halt() }
}
```

Capability trampolines read board-level data from `&self` fields — no
constants as method arguments (multi-platform invariant).

### StagePlan

Codegen emits a `static STAGE_PLAN` in `.rodata` — the capability sequence
resolved to `DeviceId` constants:

```rust
static STAGE_PLAN: fstart_stage_runtime::StagePlan = StagePlan {
    stage_name: "bootblock",
    is_first_stage: true,
    ends_with_jump: true,
    caps: &[
        CapOp::ConsoleInit(0),
        CapOp::BootMediaStatic { device: None, offset: 0x2000_0000, size: 0x200_0000 },
        CapOp::SigVerify,
        CapOp::FdtPrepare,
        CapOp::PayloadLoad,
    ],
    persistent_inited: &[],
    boot_media_gated: &[],
    all_devices: &[0],
};
```

### Init Sequence (run_stage executor)

The handwritten executor in `fstart-stage-runtime` iterates the plan's
capability sequence and dispatches through the `Board` trait:

```rust
// fstart-stage-runtime/src/lib.rs (simplified)
pub fn run_stage<B: Board>(mut board: B, plan: &'static StagePlan, handoff: usize) -> ! {
    let mut inited = DeviceMask::from_slice(plan.persistent_inited);
    for op in plan.caps {
        match *op {
            CapOp::ConsoleInit(id) => {
                if board.init_device(id).is_err() { board.halt(); }
                unsafe { board.install_logger(id); }
                inited.set(id);
            }
            CapOp::SigVerify => board.sig_verify(),
            CapOp::PayloadLoad => board.payload_load(),
            // ... one arm per CapOp variant
            _ => {}
        }
    }
    board.halt();
}
```

`fstart_main` is a thin stub:

```rust
#[no_mangle]
pub extern "Rust" fn fstart_main(handoff_ptr: usize) -> ! {
    fstart_stage_runtime::run_stage(
        _BoardDevices::new(),
        &STAGE_PLAN,
        handoff_ptr,
    )
}
```

### Bus Ordering (Approach A — compile-away)

For bus hierarchies, codegen enforces **parent before child** ordering (matching
both coreboot's tree-walk and U-Boot's recursive-probe pattern).  Parent variable
names are resolved at build time — the generated code uses direct Rust borrows:

```rust
// Generated — parent first (root device, uses Device::new):
let i2c0 = DesignwareI2c::new(&DesignwareI2cConfig {
    base_addr: 0x1004_0000,
    clock_freq: 100_000_000,
}).unwrap_or_else(|_| halt());
i2c0.init().unwrap_or_else(|_| halt());

// Then child (bus device, uses BusDevice::new_on_bus):
let tpm0 = Slb9670::new_on_bus(
    &Slb9670Config { addr: 0x50 },
    &i2c0,  // ← direct borrow, resolved by codegen
).unwrap_or_else(|_| halt());
tpm0.init().unwrap_or_else(|_| halt());
```

This is approach A: the bus hierarchy compiles away entirely.  No runtime device
lookup, no `DEVICE_TREE` traversal needed for init.  The `DEVICE_TREE` table
(approach B) exists in parallel for runtime introspection only.

### Device Tree Table (Approach B — runtime introspection)

Codegen also emits a `static DEVICE_TREE` array for runtime use cases like
power sequencing and diagnostics:

```rust
// Generated alongside the init code (same order as the RON devices list):
static DEVICE_TREE: [fstart_types::DeviceNode; 3] = [
    fstart_types::DeviceNode { parent: None, depth: 0 },          // [0] uart0
    fstart_types::DeviceNode { parent: None, depth: 0 },          // [1] i2c0
    fstart_types::DeviceNode { parent: Some(1), depth: 1 },       // [2] tpm0
];
```

### Codegen Validation

The codegen phase performs these checks at `build.rs` time, emitting
`compile_error!()` in the generated source for any failure:

| Check | Error |
|-------|-------|
| Capability references unknown device name | `"ConsoleInit references device 'foo' which is not declared"` |
| Device declares unknown driver | `"unknown driver variant ..."` |
| RON config has wrong fields for driver | serde parse error at build time |
| Capability uses device that doesn't provide required service | `"ConsoleInit requires Console service but device 'gpio0' does not provide it"` |
| Child device's parent doesn't provide a bus service | `"Device 'tpm0' has parent 'gpio0' which does not provide a bus service (I2cBus, SpiBus, ...)"` |

Note: with nested `children` in RON, some errors from the old flat `parent`
model are structurally impossible — there is no way to reference a nonexistent
parent, and the DFS flattening guarantees topological order.

## Layer 5 -- Dispatch Mode

All boards use **Rigid** mode (`BuildMode::Rigid`).  The `Board` trait is
`Sized` and the executor is generic over `B: Board`, so every call
monomorphises — the compiled firmware is identical in shape to what direct
inlined calls would produce.  **Zero vtables, zero runtime overhead.**

The earlier **Flexible** mode (enum dispatch for runtime driver selection)
was removed when the stage-runtime / codegen split landed.  The `Board`
trait makes Flexible redundant: if runtime driver selection is ever needed,
the codegen can emit `Option<enum-of-driver-variants>` fields inside
`_BoardDevices` without changing the trait, the executor, or any capability
helper.  The `BuildMode` enum retains only the `Rigid` variant for forward
compatibility.

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

To add a new root-level driver (e.g., a SiFive UART):

1. **Create the module**: `fstart-drivers/src/uart/sifive.rs`, feature-gated under
   `sifive-uart`.

2. **Define registers** with `register_structs!` / `register_bitfields!`:
   ```rust
   register_bitfields! [u32,
       TXDATA [ FULL OFFSET(31) NUMBITS(1) [], DATA OFFSET(0) NUMBITS(8) [] ],
       RXDATA [ EMPTY OFFSET(31) NUMBITS(1) [], DATA OFFSET(0) NUMBITS(8) [] ],
   ];
   ```

3. **Define the config type** with serde derives:
   ```rust
   #[derive(Debug, Clone, Serialize, Deserialize)]
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

6. **Add variant to `DriverInstance`** in `fstart-drivers/src/lib.rs`:
   ```rust
   pub enum DriverInstance {
       // ...existing variants...
       SifiveUart(SifiveUartConfig),
   }
   ```

7. **Register in codegen**: add the driver to `KNOWN_DRIVER_META` in
   `fstart-codegen/src/stage_gen/registry.rs` with its type path, config type,
   and service list.

8. **Add feature flag**: in `fstart-drivers/Cargo.toml` and `fstart-stage/Cargo.toml`.

9. **Test**: write a board RON using the driver and verify with
   `cargo xtask build --board <name>`.

### Bus Device Author Checklist

For a device that lives on a parent bus (e.g., an I2C-attached TPM):

1. **Create the module** in the appropriate category (e.g.,
   `fstart-drivers/src/tpm/slb9670.rs`), feature-gated.

2. **Define the config type** — only bus-specific fields (no `base_addr`):
   ```rust
   #[derive(Debug, Clone, Serialize, Deserialize)]
   pub struct Slb9670Config {
       pub addr: u8,  // I2C address
   }
   ```

3. **Implement `BusDevice`** (not `Device`):
   ```rust
   impl<B: I2c> BusDevice for Slb9670<B> {
       type Bus = B;
       type Config = Slb9670Config;
       fn new_on_bus(config: &Slb9670Config, bus: &B) -> Result<Self, DeviceError> { ... }
       fn init(&self) -> Result<(), DeviceError> { ... }
   }
   ```

4. **Add variant to `DriverInstance`** and register in codegen (same as above).

5. **Use nested `children` in board RON** — the device appears under its
   parent controller:
   ```ron
   (
       name: "i2c0",
       driver: DesignwareI2c(( base_addr: 0x10040000, ... )),
       services: ["I2cBus"],
       children: [
           ( name: "tpm0", driver: Slb9670(( addr: 0x50 )), services: ["Tpm"] ),
       ],
   )
   ```

## Implementation Phases

### Phase 1: Foundation ✓

- [x] Add `Device` trait (with associated `type Config`) and `DeviceError` to
      `fstart-services`.
- [x] Add `Ns16550Config`, `Pl011Config`, `DesignwareI2cConfig` structs to
      `fstart-drivers`.
- [x] Implement `Device` for `Ns16550`, `Pl011`, `DesignwareI2c`.
- [x] Add `BusDevice` trait to `fstart-services` (standalone trait with
      `new_on_bus(config, bus)`, not a subtrait of `Device`).
- [x] Add `DeviceId`, `DeviceNode` to `fstart-types` for flat index-based
      device tree.
- [x] Replace `Resources` bag-of-options with typed `DriverInstance` enum.
- [x] Remove `compatible` string matching — `DriverInstance` variant name
      determines the driver.

### Phase 2: Codegen Upgrade ✓ (superseded by Phase 6 — stage-runtime split)

- [x] ~~Generate `Devices` struct with concrete typed fields.~~ → replaced by `_BoardDevices`
- [x] ~~Generate `StageContext` with service accessor methods.~~ → removed (executor needs no context struct)
- [x] Generate typed `Config` construction via `ConfigTokenSerializer`
      (custom serde Serializer → TokenStream, supports nearly full serde
      data model).
- [x] Wire `fstart_capabilities::console_init()` into generated code.
- [x] Add codegen validation: unknown drivers, service mismatches, bus
      service checks → `compile_error!`.
- [x] Split `stage_gen.rs` into focused submodules: `registry.rs`,
      `topology.rs`, `capabilities.rs`, `flexible.rs`, `config_ser.rs`.

### Phase 3: Capability Pipeline ✓

- [x] Implement capability functions that accept service trait references.
- [x] Generate full init pipeline from capability list.
- [x] Add ordering validation (ConsoleInit must precede MemoryInit, etc.).
- [x] Multi-stage support: bootblock → main stage loading, signature
      verification, FFS integration.

### Phase 4: Bus Support ✓ (infrastructure)

- [x] Add bus service traits (`I2cBus`, `SpiBus`, `GpioController`) to
      `fstart-services` (I2C uses `embedded-hal v1.0` traits).
- [x] Implement codegen support for nested `children` in RON — DFS pre-order
      flattening guarantees parent-before-child ordering.
- [x] Generate `BusDevice::new_on_bus(&config, &parent)` for bus children
      (approach A: compile-away).
- [x] Generate `static DEVICE_TREE: [DeviceNode; N]` table for runtime
      introspection (approach B: flat index table).
- [x] Implement `validate_device_tree()` — checks bus service requirements
      (no topo sort needed, ordering is structural from DFS).
- [x] Implement DesignWare I2C bus controller driver.
- [ ] Implement first bus-attached child driver (e.g., SLB9670 TPM).
- [ ] Exercise `children` syntax in a real board RON file.

### Phase 5: Flexible Mode — REMOVED

Flexible mode was implemented and then **removed** when the stage-runtime /
codegen split landed.  The generic `Board` trait makes enum dispatch
redundant — if runtime driver selection is needed, it lives inside
`_BoardDevices` fields, not in a separate codegen mode.

- [x] ~~Implement enum-dispatch codegen for `mode: Flexible`.~~ → deleted (`flexible.rs` removed)
- [x] ~~Generate `ConsoleDevice` / `I2cBusDevice` / etc. enums.~~ → deleted
- [x] ~~Implement the service traits on the generated enums.~~ → deleted

### Phase 6: Stage Runtime / Codegen Split ✓

- [x] Create `fstart-stage-runtime` crate with `Board` trait, `StagePlan`,
      `CapOp`, `DeviceMask`, `BootMediaState`, and `run_stage<B: Board>()` executor.
- [x] `plan_gen.rs`: emit `static STAGE_PLAN: StagePlan` per stage.
- [x] `board_gen.rs`: emit `struct _BoardDevices` + `impl Board for _BoardDevices`
      with all 20 methods (init_device, init_all_devices, install_logger,
      15 capability trampolines, halt, jump_to, jump_to_with_handoff).
- [x] Flip `fstart_main` to a 3-line stub calling `run_stage()`.
- [x] Delete old codegen: `Devices`, `StageContext`, `flexible.rs`,
      `ensure_device_ready`, `walk_to_real_parent`, `make_prelude`,
      `generate_driver_init`, `generate_boot_media_auto_device`.
- [x] 25 host-side executor tests via `MockBoard`.
- [x] All 16 boards build; all 93 tests pass.
