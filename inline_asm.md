# # ACPI Generation Design

## Problem Statement

Firmware must produce ACPI tables (RSDP, XSDT, DSDT, SSDT, MADT, SPCR, etc.)
for the OS. Coreboot's two approaches each have problems:

**DSDT (static):** `.asl` files composed via C preprocessor `#include`, compiled
by Intel's `iasl` to AML bytecode. Correct but inflexible — the C preprocessor is
the only parameterization mechanism.

**SSDT (runtime):** ~150 `acpigen_*` C functions that manually emit AML bytecode
into a shared global buffer. Flexible but painful:

- Manual PkgLength patching — reserves 3 bytes, then `memmove()`s entire payload
  when fewer bytes needed. O(n) per scope close, cascading for nested structures.
- Unenforceable push/pop discipline — every `write_scope()`/`write_device()` must
  pair with exactly one `pop_len()`. Miss one and all subsequent output corrupts.
  No compile-time enforcement.
- Raw opcode knowledge still required — many codepaths emit literal opcode bytes
  (`LEQUAL_OP`, `LOCAL0_OP`). Some functions bypass the API entirely with
  hardcoded byte arrays (e.g. `acpigen_write_empty_PCT()`).
- No validation — names, argument counts, package element counts, resource template
  well-formedness — none checked.
- Global mutable state — single write cursor, single nesting stack, no thread
  safety, no composability, no rollback.
- Package element count is manually tracked — callers get a `char *` to the count
  byte and `(*pkg_count)++` it by hand.

**Goal for fstart:** A single mechanism that works for both static (build-time)
and dynamic (runtime) generation, with compile-time validation, good ergonomics,
and the ability to dry-run on the host.

## Existing Rust Ecosystem

### `acpi_tables` v0.2.0 (rust-vmm) — THE generation crate

Repository: https://github.com/rust-vmm/acpi_tables
Origin: Cloud Hypervisor / Rivos. Production-proven in VMMs.

| Property | Status |
|---|---|
| `no_std` | Yes, but requires `alloc` (deeply structural — see below) |
| AML coverage | Good for hypervisors. See "gaps" below |
| Static tables | 20+ types incl. RHCT (RISC-V, Rivos), MADT (GIC/APLIC/PLIC/IMSIC), SPCR |
| API pattern | Tree-of-references: `Device::new(path, vec![&child1, &child2])` |
| PkgLength | Correct by construction — serialize children to temp Vec first, compute exact length, write prefix. No backpatching |
| Dependencies | `zerocopy` only |

**Core traits:**

```rust
pub trait AmlSink {
    fn byte(&mut self, byte: u8);            // required
    fn word(&mut self, word: u16) { ... }    // default impl
    fn dword(&mut self, dword: u32) { ... }
    fn qword(&mut self, qword: u64) { ... }
    fn vec(&mut self, v: &[u8]) { ... }
}

pub trait Aml {
    fn to_aml_bytes(&self, sink: &mut dyn AmlSink);
}
```

Provided `AmlSink` impls: `Vec<u8>`, `Checksum`, `Sdt`, `PackageBuilder`.

**AML builder structs** (all implement `Aml`):

- Scoping: `Scope`, `Device`, `Method`, `PowerResource`
- Named objects: `Name`, `OpRegion`, `Field`, `Mutex`
- Control flow: `If`, `Else`, `While`, `Return`
- Operators: `Equal`/`NotEqual`/`LessThan`/`GreaterThan`/`LessEqual`/`GreaterEqual`,
  `Add`/`Subtract`/`Multiply`/`Mod`, `And`/`Or`/`Xor`/`Nand`/`Nor`,
  `ShiftLeft`/`ShiftRight`, `Index`, `CreateDWordField`/`CreateQWordField`
- Data: `Path`, `EISAName`, `Uuid`, `BufferTerm`/`BufferData`,
  `Package`/`PackageBuilder`/`VarPackageTerm`
- References: `Arg(0-6)`, `Local(0-7)`, `Store`, `DeRefOf`, `ObjectType`, `SizeOf`
- Operations: `MethodCall`, `Notify`, `Acquire`, `Release`, `Mid`, `CreateField`,
  `Concat`, `ConcatRes`, `ToString`, `ToBuffer`, `ToInteger`
- Resources: `Memory32Fixed`, `AddressSpace<u16/u32/u64>`, `IO`, `Interrupt`,
  `Register`, `ResourceTemplate`

**Static table structs:**
RSDP, XSDT, FADT (builder pattern), MADT, MCFG, SRAT, SLIT, PPTT, SPCR, BERT,
CEDT, FACS, HEST, HMAT, RHCT, RIMT, RQSC, TPM2, VIOT, plus generic `Sdt`.

**Why alloc is deeply needed:** Every compound AML object (Scope, Device, Method,
If, Package, etc.) serializes its children into a *temporary* `Vec<u8>` to measure
their total byte count *before* writing the PkgLength prefix. This is
architecturally fundamental — AML's variable-width PkgLength encoding (1-4 bytes)
means you must know child size before writing the parent header. The only
alternatives are coreboot-style backpatching or a two-pass measure/emit approach.

**Gaps for firmware use:**

Missing AML constructs:
- `ThermalZone`, `Processor` (deprecated but used)
- `IndexField`, `BankField`
- `Sleep`, `Stall`, `Break`, `Continue`
- `CondRefOf`, `RefOf`
- `Increment`, `Decrement`
- `Wait`, `Signal`, `Event`

Missing resource descriptors:
- GPIO Connection Descriptor (GpioIo, GpioInt) — essential for SoC platforms
- I2C/SPI/UART Serial Bus Connection Descriptors — essential for embedded
- IRQ Descriptor (legacy small form)
- DMA Descriptor

These are straightforward additions — the crate's internal `binary_op!` /
`object_op!` / `compare_op!` macros make extension mechanical.

**Compile-time generation: not possible.** The `Aml` trait uses `&mut dyn AmlSink`
(dynamic dispatch), incompatible with `const fn`. Generating AML at compile time
requires either a proc-macro or a build.rs step.

### Other crates (not useful for generation)

| Crate | Direction | Notes |
|---|---|---|
| `acpi` 6.1.0 (rust-osdev) | Parse | OS-side table parser + AML interpreter. Useful for test validation |
| `aml` 0.16.4 (rust-osdev) | Parse | AML bytecode parser only |
| `raw-acpi` 0.0.2 | Struct defs | Very immature, no logic |
| `libacpica` / `acpica-sys` | C FFI | Wraps ACPICA reference impl. Not suitable for pure-Rust firmware |

### Ecosystem gap

**No proc-macro or DSL approach for ACPI exists in Rust.** Zero results across
crates.io, lib.rs, and GitHub. No inline ASL → bytecode transformation. This is
novel work.

## Architecture

```
Layer 3:  Board RON ──► fstart-codegen/acpi_gen.rs ──► embedded ACPI byte arrays
             │               (build.rs, host-side)
             │
Layer 2:  acpi_dsl! { ... }  ◄── fstart-acpi-macros   (proc-macro crate)
             │                    Parses Rust-flavored ASL DSL
             │                    Validates names, args, nesting at compile time
             │                    Emits builder calls targeting Layer 1
             │
Layer 1:  fstart-acpi         ◄── AML builder types + AmlSink trait
             │                    Wraps/re-exports acpi_tables
             │                    Adds missing descriptors (GPIO, I2C, SPI)
             │                    Adds missing ops (Sleep, ThermalZone, etc.)
             │
          Target: .to_aml_bytes() into firmware memory buffer
          Host:   .to_aml_bytes() into Vec<u8>, validated with iasl -d
```

### Layer 1: `fstart-acpi`

Depends on `acpi_tables`, re-exports its public API, and extends it with:

- Missing resource descriptors (GPIO, I2C, SPI serial bus)
- Missing AML operations (Sleep, ThermalZone, CondRefOf, etc.)
- `FixedBufSink` — `AmlSink` impl backed by `&mut [u8]` + index (no alloc for
  the final output path, though builder internals still allocate)
- Static table construction helpers driven by fstart's `BoardConfig` types

The `alloc` requirement is acceptable because ACPI tables are built after
`MemoryInit` capability (which sets up the allocator). The `AmlSink` trait itself
can target any buffer, including a bump-allocated region.

### Layer 2: `acpi_dsl!` Proc-Macro

A proc-macro accepting Rust-flavored ASL syntax with `#{expr}` interpolation.
Emits Rust code that constructs `fstart-acpi` builder types and serializes them.

**Syntax design principles:**
- Must be valid Rust token streams (parsed by rustc's tokenizer, then by syn)
- Keywords are Rust identifiers: `scope`, `device`, `method`, `name`, `ret`, etc.
- ACPI paths are string literals: `"\\_SB"`, `"PCI0"`, `"_HID"`
- Interpolation uses `#{rust_expr}` (no conflict with ASL syntax — ASL has no `#`)
- Braces `{}` for nesting (natural for Rust developers)
- Semicolons `;` terminate leaf statements

**Example — full DSDT:**

```rust
use fstart_acpi_macros::acpi_dsl;

let uart_base: u64 = board.devices[0].resources.mmio_base.unwrap();
let uart_irq: u32 = 10;

let dsdt_bytes: Vec<u8> = acpi_dsl! {
    definition_block("dsdt.aml", "DSDT", 2, "FSTRT", "BOARD1", 0x1) {
        scope("\\_SB") {
            device("PCI0") {
                name("_HID", eisa_id("PNP0A08"));
                name("_CID", eisa_id("PNP0A03"));
                name("_UID", 1u32);

                method("_OSC", 4, Serialized) {
                    create_dword_field(Arg3, 0, CDW1);
                    if_expr(Arg0 == to_uuid("33DB4D5B-...")) {
                        create_dword_field(Arg3, 4, CDW2);
                        store(CDW2, SUPP);
                        and(CTRL, 0x1Du32, CTRL);
                    } else_expr {
                        or(CDW1, 4u32, CDW1);
                    }
                    ret(Arg3);
                }

                device("UAR0") {
                    name("_HID", eisa_id("PNP0501"));
                    name("_UID", 0u32);

                    method("_STA", 0, NotSerialized) {
                        ret(0x0Fu32);
                    }

                    name("_CRS", resource_template {
                        memory_32_fixed(ReadWrite, #{uart_base}, 0x1000u32);
                        interrupt(ResourceConsumer, Level, ActiveHigh,
                                  Exclusive, #{uart_irq});
                    });
                }
            }
        }

        scope("\\_GPE") {
            name("_HID", "ACPI0006");
        }
    }
};
```

**What the macro emits** (conceptual, for the UAR0 device):

```rust
{
    use fstart_acpi::aml::*;

    let __hid = Name::new("_HID".into(), &EISAName::new("PNP0501"));
    let __uid = Name::new("_UID".into(), &0u32);

    let __sta_ret = Return::new(&0x0Fu32);
    let __sta = Method::new("_STA".into(), 0, false, vec![&__sta_ret]);

    let __mem = Memory32Fixed::new(true, uart_base as u32, 0x1000u32);
    let __irq = Interrupt::new(true, true, false, false, uart_irq);
    let __crs_rt = ResourceTemplate::new(vec![&__mem, &__irq]);
    let __crs = Name::new("_CRS".into(), &__crs_rt);

    let __dev = Device::new("UAR0".into(), vec![
        &__hid, &__uid, &__sta, &__crs,
    ]);

    let mut __sink = Vec::new();
    __dev.to_aml_bytes(&mut __sink);
    __sink
}
```

The macro generates `let` bindings in leaf-to-root order (required because
`acpi_tables` uses `&dyn Aml` references that must outlive their parents), then
assembles the tree.

**Compile-time validation:**

| Check | How |
|---|---|
| ACPI name validity | 1-4 chars, `[A-Z0-9_]` only. Predefined `_XXX` names checked against known set |
| Method arg count | 0-7, cross-checked against Arg0-Arg6 usage in body |
| Scope nesting | No duplicate names at same scope level |
| Resource template | Descriptor types valid within context |
| Type hints | `_HID` must be string or EisaId, `_UID` must be integer or string, `_CRS` must be resource_template |

### Layer 3: Board RON Integration

New module `acpi_gen.rs` in `fstart-codegen`, alongside existing `stage_gen.rs`
and `linker.rs`:

```rust
// fstart-codegen/src/acpi_gen.rs

/// Generate all ACPI tables for a board, returning raw bytes per table.
pub fn generate_acpi_tables(config: &BoardConfig) -> Vec<(TableId, Vec<u8>)> {
    let mut tables = Vec::new();

    // DSDT — device tree derived from board config
    tables.push(("DSDT", generate_dsdt(config)));

    // Static tables derived from board config
    match config.platform {
        Platform::Riscv64 => {
            tables.push(("RHCT", generate_rhct(config)));
        }
        Platform::Aarch64 => {
            tables.push(("MADT", generate_madt_gic(config)));
        }
    }

    // SPCR — serial port console redirection (from console device)
    if let Some(console) = find_console_device(config) {
        tables.push(("SPCR", generate_spcr(config, console)));
    }

    tables
}

fn generate_dsdt(config: &BoardConfig) -> Vec<u8> {
    // Walk config.devices, emit Device nodes with _HID/_UID/_CRS
    // derived from each device's driver type and resources.
    // Uses acpi_tables builder API directly (not the macro — codegen
    // emitting macro invocations is unnecessarily indirect).
}
```

The build.rs pipeline becomes:

```
board.ron
  → ron_loader::load_board_config()     → BoardConfig
  → stage_gen::generate_stage_source()  → generated_stage.rs
  → linker::generate_linker_script()    → link.ld
  → acpi_gen::generate_acpi_tables()    → generated_acpi.rs
      (contains: const DSDT_AML: &[u8] = &[...];
                 const RHCT_BYTES: &[u8] = &[...]; etc.)
```

The generated stage code gains an `AcpiPrepare` capability that writes these
pre-built byte arrays into the ACPI region, then collects runtime SSDT
contributions from drivers.

**Board RON extension** (optional `acpi` section):

```ron
(
    name: "qemu-riscv64",
    // ... existing fields ...
    acpi: Some((
        oem_id: "FSTRT",
        oem_table_id: "QEMURV64",
        tables: [Dsdt, Rhct, Spcr],
    )),
)
```

### Runtime SSDT Generation

For devices discovered at runtime (PCI enumeration, hot-plug), drivers implement
an ACPI contribution trait:

```rust
// In fstart-services
pub trait AcpiDevice {
    /// Emit this device's SSDT contribution into the sink.
    fn fill_ssdt(&self, sink: &mut dyn AmlSink);
}
```

Driver implementations use `acpi_dsl!` or the builder API directly:

```rust
// In fstart-drivers, e.g. ns16550
impl AcpiDevice for Ns16550 {
    fn fill_ssdt(&self, sink: &mut dyn AmlSink) {
        let base = self.base_addr;
        let irq = self.irq;
        let bytes = acpi_dsl! {
            device("UAR0") {
                name("_HID", eisa_id("PNP0501"));
                name("_CRS", resource_template {
                    memory_32_fixed(ReadWrite, #{base}, 0x1000u32);
                    interrupt(ResourceConsumer, Level, ActiveHigh,
                              Exclusive, #{irq});
                });
            }
        };
        sink.vec(&bytes);
    }
}
```

The generated `fstart_main()` collects all SSDT contributions during the
`AcpiPrepare` capability phase.

## Testing & Dry-Run Strategy

All ACPI generation runs identically on host and target — the `fstart-acpi` and
`fstart-acpi-macros` crates are `std`-capable (like `fstart-types` and
`fstart-codegen`).

### Unit tests (in fstart-codegen, fstart-acpi)

```rust
#[test]
fn test_uart_device_aml() {
    let bytes = acpi_dsl! {
        device("UAR0") {
            name("_HID", eisa_id("PNP0501"));
            name("_UID", 0u32);
        }
    };
    assert_eq!(bytes, include_bytes!("testdata/uart_device.aml"));
}
```

### Round-trip validation via iasl

```rust
#[test]
fn test_dsdt_round_trip() {
    let dsdt = generate_dsdt(&test_board_config());
    let tmp = tempdir().unwrap();
    std::fs::write(tmp.path().join("dsdt.aml"), &dsdt).unwrap();
    let out = Command::new("iasl")
        .args(["-d", "dsdt.aml"])
        .current_dir(tmp.path())
        .output().unwrap();
    assert!(out.status.success(),
        "iasl: {}", String::from_utf8_lossy(&out.stderr));
}
```

### Parse-back validation

Use the `acpi` (rust-osdev) crate as an independent verifier — generate tables
with `fstart-acpi`, parse them back, assert structural equivalence.

### Macro expansion tests

`trybuild` crate for compile-fail tests (invalid ACPI names, bad arg counts).
`cargo expand` for manual inspection of generated builder code.

### QEMU integration

Build a board, boot in QEMU, use Linux's `/sys/firmware/acpi/tables/` to dump
and verify tables match expectations.

## Crate Structure

```
crates/
  fstart-acpi/                  # no_std + alloc, target + host
    Cargo.toml                  # dep: acpi_tables
    src/
      lib.rs                    # re-exports acpi_tables + extensions
      sink.rs                   # FixedBufSink impl
      descriptors/
        gpio.rs                 # GpioIo / GpioInt connection descriptor
        i2c.rs                  # I2C serial bus descriptor
        spi.rs                  # SPI serial bus descriptor
      ext/
        thermal_zone.rs         # ThermalZone AML construct
        sleep.rs                # Sleep / Stall
        cond_ref_of.rs          # CondRefOf / RefOf

  fstart-acpi-macros/           # proc-macro crate (host only)
    Cargo.toml                  # dep: syn, quote, proc-macro2
    src/
      lib.rs                    # #[proc_macro] acpi_dsl, resource_template
      parse/
        mod.rs
        scope.rs                # Scope, Device, PowerResource, ThermalZone
        method.rs               # Method, Return, control flow
        name.rs                 # Name, Package, Buffer
        resource.rs             # ResourceTemplate descriptors
        field.rs                # OpRegion, Field
        expr.rs                 # Expressions, operators, #{} interpolation
      validate/
        mod.rs
        names.rs                # ACPI name validation
        methods.rs              # Arg count / Local usage checks
      emit/
        mod.rs                  # TokenStream gen targeting fstart-acpi types

  fstart-codegen/
    src/
      acpi_gen.rs               # NEW — BoardConfig → ACPI table bytes
      # existing: lib.rs, ron_loader.rs, stage_gen.rs, linker.rs

  fstart-services/
    src/
      acpi.rs                   # NEW — AcpiDevice trait
```

## Implementation Phases

### Phase 1 — Foundation

- Create `fstart-acpi` crate wrapping `acpi_tables`
- Implement `FixedBufSink`
- Add `acpi_gen.rs` to codegen with RHCT + SPCR generation from board RON
- Unit tests with iasl round-trip validation
- No macro yet — pure builder API

### Phase 2 — Proc-Macro DSL (core subset)

- Create `fstart-acpi-macros`
- Parser for: `definition_block`, `scope`, `device`, `name`, `method`, `ret`,
  `resource_template`, `memory_32_fixed`, `interrupt`
- `#{expr}` interpolation
- ACPI name validation
- `trybuild` tests + golden-file AML comparison

### Phase 3 — DSDT Generation

- Extend DSL: `if_expr`/`else_expr`, `store`, `op_region`, `field`, operators
- Add GPIO/I2C/SPI descriptors to `fstart-acpi`
- Generate DSDT from board RON in `acpi_gen.rs`
- Board RON `acpi` section

### Phase 4 — Runtime SSDT & Driver Integration

- `AcpiDevice` trait in `fstart-services`
- Codegen emits SSDT collection phase in `fstart_main()`
- Drivers implement `fill_ssdt()` using `acpi_dsl!`
- MADT generation

### Phase 5 — Advanced

- Full control flow (While, Break, Mutex, Notify, Sleep)
- ThermalZone, PowerResource
- _DSM method framework
- Device property (_DSD) helper API

## Design Decisions

### Alloc is acceptable

`acpi_tables` requires alloc for PkgLength computation (every compound AML object
buffers children into a temp `Vec<u8>` to measure size before writing the parent
length prefix). The only alternatives are coreboot-style backpatching (fragile) or
a two-pass measure/emit rewrite (significant effort for marginal benefit).

ACPI tables are built after MemoryInit, so an allocator is available. Accepted.

### Proc-macro over macro_rules!

`macro_rules!` could handle basic syntax but cannot validate ACPI names, cross-
reference method argument usage, or produce good error spans. The proc-macro
(syn + quote) gives compile-time validation with precise error locations. Worth
the extra crate.

### Wrap acpi_tables, don't fork

Starting as a thin wrapper with extension types is faster and maintains upstream
compatibility. A fork only becomes necessary if upstream rejects contributions or
the alloc dependency must be eliminated. Contribute missing descriptors upstream
where possible.

### Build-time DSDT, runtime SSDT

DSDT is generated at build time from board RON and embedded as `&[u8]`. This
matches coreboot's model (static DSDT in CBFS) and gives fastest boot. SSDT is
generated at runtime for discoverable devices. The `acpi_dsl!` macro works in
both contexts — the same syntax in `build.rs` codegen and in driver code.


