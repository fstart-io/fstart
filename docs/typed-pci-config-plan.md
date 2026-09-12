# Typed PCI Configuration Access Plan

> **Status:** the address, header, capability, and config-access vocabulary is
> provided by the upstream `pci_types` crate. References below to defining
> fstart-specific `PciBdf`, `PciSbdf`, or config-access traits are superseded;
> fstart retains only resource-window/allocation types and chipset-specific
> register overlays.

## Motivation

The ICH7/ICH8 RCBA work replaced raw offset helpers and manual read-modify-write
patterns with `tock-registers` register overlays and typed `.modify(...)` calls.
PCI configuration space should move in the same direction, but PCI has two
additional complications:

1. Many registers are standardized by the PCI specification and should not be
   redefined in every driver.
2. Many registers are device- or chipset-specific and must remain close to the
   driver that understands them.

There is also duplicated PCI vocabulary today across:

- `crates/fstart-services/src/pci.rs`
  - `PciAddr`
  - `PciWindow`
  - `PciWindowKind`
  - `PciRootBus`
  - standard PCI offsets and bit constants
- `crates/fstart-ecam/src/lib.rs`
  - global ECAM base
  - `PciDevBdf`
  - raw config-space access helpers
- `crates/fstart-driver-pci-ecam`
  - enumeration and resource allocation
- chipset drivers
  - direct raw `read8`/`write8`/`or16`/`modify32` calls

The goal is to consolidate generic PCI definitions while enabling RCBA-style
typed access for both standard and device-specific PCI config registers.

## Design Goals

- Use `tock-registers` for typed PCI config access.
- Avoid hand-written boilerplate for standard PCI header fields.
- Keep device-specific register definitions in the relevant driver crate.
- Avoid a visible `regs.common.command` split at call sites.
- Keep raw accessors available for hardware sequences that require exact writes,
  BAR sizing, capability walking, or errata-specific behavior.
- Reduce duplication between `fstart-services`, `fstart-ecam`, and PCI drivers.
- Preserve `no_std` compatibility.

## Proposed Crate Split

### `fstart-pci`

Add a new shared crate for generic PCI vocabulary and standard layout details.

It should own:

- canonical BDF/address type
- PCI resource window types
- standard PCI config-space offsets
- standard PCI command/status/header constants
- standard PCI `tock-registers` bitfields
- macros for generating flat Type 0 / Type 1 config structs
- capability IDs and traversal helpers where appropriate

Example modules:

```text
crates/pci/
  src/lib.rs
  src/addr.rs
  src/window.rs
  src/config.rs
  src/capability.rs
  src/macros.rs
```

### `fstart-services`

Keep service traits here, but stop owning generic PCI constants and data types.

`fstart-services::pci` should eventually contain mainly:

```rust
pub use fstart_pci::{PciBdf, PciWindow, PciWindowKind};

pub trait PciRootBus: Send + Sync {
    fn init_bus(&mut self) -> Result<(), ServiceError>;
    fn config_read32(&self, addr: PciBdf, reg: u16) -> Result<u32, ServiceError>;
    fn config_write32(&self, addr: PciBdf, reg: u16, val: u32) -> Result<(), ServiceError>;
    // convenience read8/read16/write8/write16 defaults
    // segment/window/device-count metadata
}
```

During migration, `fstart-services::pci` can re-export old names and constants so
existing users do not all need to move at once.

### `fstart-ecam`

`fstart-ecam` should be the ECAM access backend, not the owner of PCI concepts.
It should use the canonical `fstart_pci::PciBdf` type.

It should provide:

```rust
pub fn init(base: usize);
pub fn base() -> usize;

pub struct EcamDevice {
    bdf: PciBdf,
}

impl EcamDevice {
    pub const fn new(bus: u8, dev: u8, func: u8) -> Self;
    pub const fn bdf(&self) -> PciBdf;

    pub fn read8(&self, reg: u16) -> u8;
    pub fn read16(&self, reg: u16) -> u16;
    pub fn read32(&self, reg: u16) -> u32;
    pub fn write8(&self, reg: u16, val: u8);
    pub fn write16(&self, reg: u16, val: u16);
    pub fn write32(&self, reg: u16, val: u32);

    pub fn type0_regs(&self) -> &'static PciType0Config;
    pub fn type1_regs(&self) -> &'static PciType1Config;

    pub unsafe fn regs<T>(&self) -> &'static T;
}
```

For compatibility, `PciDevBdf` can initially remain as a type alias or wrapper.
Long term, there should be one canonical BDF type.

## Canonical PCI Address Type

Replace the current split between `PciAddr` and `PciDevBdf` with one type in
`fstart-pci`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PciBdf {
    pub bus: u8,
    pub dev: u8,
    pub func: u8,
}

impl PciBdf {
    pub const fn new(bus: u8, dev: u8, func: u8) -> Self {
        Self { bus, dev, func }
    }
}
```

Migration aliases:

```rust
pub type PciAddr = PciBdf;
pub type PciDevBdf = EcamDevice; // or temporary wrapper, depending on migration needs
```

## Flat Standard PCI Config Structs

Avoid a visible split such as:

```rust
regs.common.command.modify(...);
regs.device_specific_reg.modify(...);
```

Instead, generated PCI config structs should be flat:

```rust
regs.command.modify(PCI_COMMAND::BUS_MASTER::SET);
regs.vendor_id.get();
regs.gen_pmcon_1.modify(GEN_PMCON_1::ACPI_BASE_LOCK::SET);
```

`fstart-pci` should provide standard flat structs for generic access:

```rust
register_structs! {
    pub PciType0Config {
        (0x00 => pub vendor_id: MmioReadOnly<u16>),
        (0x02 => pub device_id: MmioReadOnly<u16>),
        (0x04 => pub command: MmioReadWrite<u16, PCI_COMMAND::Register>),
        (0x06 => pub status: MmioReadWrite<u16, PCI_STATUS::Register>),
        (0x08 => pub revision_id: MmioReadOnly<u8>),
        (0x09 => pub prog_if: MmioReadOnly<u8>),
        (0x0a => pub subclass: MmioReadOnly<u8>),
        (0x0b => pub class_code: MmioReadOnly<u8>),
        // Type 0 fields: BAR0..BAR5, subsystem IDs, interrupt line/pin, etc.
        (0x1000 => @END),
    }
}
```

And similarly for Type 1 bridge headers.

## Standard Bitfields

Standard PCI bitfields belong in `fstart-pci`, not every driver.

Example style:

```rust
register_bitfields! [u16,
    PCI_COMMAND [
        IO_SPACE OFFSET(0) NUMBITS(1) [],
        MEMORY_SPACE OFFSET(1) NUMBITS(1) [],
        BUS_MASTER OFFSET(2) NUMBITS(1) [],
        SPECIAL_CYCLES OFFSET(3) NUMBITS(1) [],
        MEM_WRITE_INVALIDATE OFFSET(4) NUMBITS(1) [],
        VGA_PALETTE_SNOOP OFFSET(5) NUMBITS(1) [],
        PARITY_ERROR_RESPONSE OFFSET(6) NUMBITS(1) [],
        SERR_ENABLE OFFSET(8) NUMBITS(1) [],
        FAST_BACK_TO_BACK OFFSET(9) NUMBITS(1) [],
        INT_DISABLE OFFSET(10) NUMBITS(1) []
    ],
    PCI_STATUS [
        CAPABILITIES_LIST OFFSET(4) NUMBITS(1) [],
        MASTER_DATA_PARITY_ERROR OFFSET(8) NUMBITS(1) [],
        SIGNALED_TARGET_ABORT OFFSET(11) NUMBITS(1) [],
        RECEIVED_TARGET_ABORT OFFSET(12) NUMBITS(1) [],
        RECEIVED_MASTER_ABORT OFFSET(13) NUMBITS(1) [],
        SIGNALED_SYSTEM_ERROR OFFSET(14) NUMBITS(1) [],
        DETECTED_PARITY_ERROR OFFSET(15) NUMBITS(1) []
    ],
]
```

## Device-Specific PCI Config Structs

Device-specific registers should live in the driver crate that owns the device.
The standard fields should still appear flat in the resulting struct.

Desired declaration style:

```rust
pci_type0_config! {
    pub struct Ich7LpcPciConfig {
        (0x40 => pub acpi_cntl: MmioReadWrite<u8, ACPI_CNTL::Register>),
        (0x44 => pub gen_pmcon_1: MmioReadWrite<u16, GEN_PMCON_1::Register>),
        (0xa0 => pub smi_lock: MmioReadWrite<u16, SMI_LOCK::Register>),
        (0x1000 => @END),
    }
}
```

This expands to one flat `register_structs!` block containing both standard PCI
header fields and the device-specific fields:

```rust
register_structs! {
    pub Ich7LpcPciConfig {
        (0x00 => pub vendor_id: MmioReadOnly<u16>),
        (0x02 => pub device_id: MmioReadOnly<u16>),
        (0x04 => pub command: MmioReadWrite<u16, PCI_COMMAND::Register>),
        (0x06 => pub status: MmioReadWrite<u16, PCI_STATUS::Register>),
        // ... more standard Type 0 fields ...
        (0x40 => pub acpi_cntl: MmioReadWrite<u8, ACPI_CNTL::Register>),
        (0x44 => pub gen_pmcon_1: MmioReadWrite<u16, GEN_PMCON_1::Register>),
        (0xa0 => pub smi_lock: MmioReadWrite<u16, SMI_LOCK::Register>),
        (0x1000 => @END),
    }
}
```

Then call sites can look like:

```rust
let regs = self.lpc_regs();

regs.command.modify(PCI_COMMAND::BUS_MASTER::SET);
regs.smi_lock.modify(SMI_LOCK::GLOBAL_SMI_LOCK::SET);
```

No `common` namespace is exposed.

## Macros Before Proc Macros

A derive macro could eventually be useful, but start with declarative macros.
They are easier to keep `no_std`-friendly and easier to audit.

Initial macro set:

```rust
pci_type0_config! { ... }
pci_type1_config! { ... }
```

Possible later derive-style API:

```rust
#[derive(PciDevice)]
#[pci(bus = 0, dev = ich7::LPC_DEV, func = ich7::LPC_FUNC)]
pub struct Ich7Lpc;
```

Defer this until several drivers prove the desired shape.

## Capability Handling

PCI capabilities are offset-linked structures, not fixed header fields. They
should not be forced into the static config struct except for the standard
`capabilities_ptr` field.

Add helpers such as:

```rust
pub fn find_capability(dev: &EcamDevice, cap_id: u8) -> Option<u16>;
pub fn find_pcie_capability(dev: &EcamDevice) -> Option<u16>;
```

Device code can then use raw offset-relative access or a future typed wrapper:

```rust
let pcie = find_pcie_capability(&dev)?;
dev.write16(pcie + PCIE_LINK_CONTROL, value);
```

## Raw Access Remains Necessary

Do not remove raw config-space methods. They are still required for:

- BAR sizing sequences
- exact write behavior copied from hardware docs or coreboot
- write-1-to-clear status registers
- read-write-back lock patterns
- capability traversal
- registers with undocumented or poorly understood semantics
- early bring-up before a typed overlay exists

Typed access should be preferred for known bitfields, but raw access is still an
escape hatch.

## Migration Plan

1. Add `fstart-pci` crate.
2. Move generic PCI data types into `fstart-pci`:
   - `PciBdf`
   - `PciWindow`
   - `PciWindowKind`
3. Move standard PCI constants from `fstart-services::pci` into `fstart-pci`.
4. Re-export moved items from `fstart-services::pci` for compatibility.
5. Update `fstart-services::PciRootBus` to use `fstart_pci::PciBdf` internally.
6. Update `fstart-ecam` to use the canonical `PciBdf` and provide `EcamDevice`.
7. Add standard Type 0 / Type 1 flat config structs in `fstart-pci`.
8. Add standard PCI bitfields in `fstart-pci`.
9. Add `pci_type0_config!` and `pci_type1_config!` macros that generate flat
   structs with standard fields plus driver-specific fields.
10. Convert obvious standard-register users first:
    - command register `0x04`
    - status register `0x06`
    - header type `0x0e`
    - class/revision fields
    - interrupt line/pin
11. Add device-specific overlays in ICH7, ICH8, GM965, and Pineview drivers one
    device at a time.
12. Replace manual PCI read-modify-write patterns with typed `.modify(...)` where
    semantics are known.
13. Remove compatibility aliases/re-exports after all call sites are migrated.

## Example End State

Current style:

```rust
let lpc = ecam::PciDevBdf::new(0, ich7::LPC_DEV, ich7::LPC_FUNC);
lpc.or16(0x04, 1 << 2);
lpc.or16(0xa0, 1 << 4);
```

Target style:

```rust
let regs = self.lpc_regs();

regs.command.modify(PCI_COMMAND::BUS_MASTER::SET);
regs.smi_lock.modify(SMI_LOCK::GLOBAL_SMI_LOCK::SET);
```

Driver-local helper:

```rust
fn lpc_regs(&self) -> &'static Ich7LpcPciConfig {
    // SAFETY: this BDF is fixed to the ICH7 LPC device and ECAM has been initialized.
    unsafe { self.lpc().regs::<Ich7LpcPciConfig>() }
}
```

## Non-Goals For The First Pass

- Do not remove raw PCI config access.
- Do not require a proc macro.
- Do not type every vendor-specific register globally.
- Do not force capability structures into fixed header overlays.
- Do not convert exact-write sequences unless the replacement is proven
  behavior-preserving.
