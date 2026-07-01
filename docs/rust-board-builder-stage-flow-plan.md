# Rust Board Builder and Fixed Stage Flow Plan

<!-- markdownlint-disable MD013 -->

> **Superseded** by [architecture.md](architecture.md) (config-as-data, fixed
> family flows). Kept for history only. The typed newtypes, configuration
> ownership rule, and validation split were carried forward; the generic
> device graph, `HardwareInit` step trait, flow feature families, ordering
> DSL, and dynamic board-blob design were dropped.

This is the current architecture direction for board authoring and stage flow.
It supersedes the previous RON/stage-codegen design documents, including the old
architecture, driver model, continuation, topology DSL, generated StagePlan, and
RON user-guide documents.

## Motivation

The current RON-based board description has two problems:

1. Board correctness is checked manually in central validation code.
2. Stage behavior is hard to read because board data is lowered into generated
   stage glue and `StagePlan` tables.

The required direction is to replace board RON authoring with normal Rust and to
make stage execution fixed, handwritten Rust flow. Board code should stay light:
it describes topology, configuration, and board-specific quirks. It does not
hand-write generic device init ordering.

## Goals

- Use plain Rust builder APIs for board/topology/configuration data.
- Make common topology mistakes impossible where Rust types can express the
  relationship, including typed child buses/ports for chipset and SoC devices.
- Avoid generated Rust stage code.
- Make stage flow readable as fixed handwritten code.
- Move hardware sequencing knowledge into driver/init crates.
- Keep mainboard code small and board-specific.
- Keep build/package information as explicit Rust metadata built with the same
  fluent style, but separate from target runtime control flow.
- Support two build modes:
  - **static typed mode**: board information is compiled into the stage.
  - **dynamic board-blob mode**: board information is added later as a binary
    blob, deserialized at boot, and matched to compiled-in drivers.
- Keep framework traits generic; avoid assumptions like `with_southbridge()` in
  common code.

## Non-goals

- Do not create a macro-heavy DSL.
- Do not turn every named flow step into an independent Cargo feature. The flow
  needs readable semantic barriers, but feature gates should be coarse enough to
  avoid a capability/feature explosion.
- Do not require a central board registry with a `match board_name` table.
- Do not make mainboards manually list `self.foo.init()?; self.bar.init()?;` for
  generic driver initialization.
- Do not keep `Device::init()` as a lifecycle API or compatibility path.
- Do not rely on undocumented numeric priorities with “no guarantees” for
  hardware ordering. Firmware ordering must be deterministic and reviewable.

## Design conclusions from this review

- **Typed child topology is worth adding**, but only as driver/platform builder
  API. A southbridge may expose typed `pci()`, `lpc()`, `smbus()`, or `spi()`
  child builders, but common framework code should still lower that to a generic
  parent/bus/device graph. Do not add global common-code concepts such as
  `with_southbridge()`.
- **Flow step names and Cargo feature names should not be one-to-one.** Keep
  semantic step names for readability and ordering, then gate them with a small
  number of feature families or board-selected flow profiles.
- **Priority numbers should not be the primary ordering mechanism.** Parent-child
  topology, typed bus ports, and explicit `before`/`after` constraints are easier
  to audit. A numeric order hint is acceptable only as a deterministic, local
  tie-breaker inside one flow step after dependencies have already been checked.
- **A derive macro can help dynamic runtime mode**, especially for the closed
  runtime-driver enum and registry boilerplate. It must remain optional; the
  underlying traits and ABI should be hand-writable and testable without macros.
- **Build metadata needs its own builder.** Host paths, Cargo features, image
  packaging, linker choices, payload files, and build profiles are board facts
  for `xtask`, not target-stage runtime logic.
  `xtask` may emit ordinary build artifacts such as `link.ld` and
  human-readable metadata, but it must not generate Rust stage code or recreate a
  central `match board_name` registry in the stage crate.

## Plain Rust builder pattern

The board description should be regular Rust using fluent builders:

```rust
pub fn board_info() -> BoardInfo {
    Board::new("qemu-riscv64")
        .platform(Platform::Riscv64)
        .memory(
            Memory::new()
                .rom("flash", 0x2000_0000, 0x0200_0000)
                .ram("ram", 0x8000_0000, 0x0800_0000),
        )
        .device(
            Device::new("uart0")
                .driver(
                    Ns16550::new()
                        .regs(mmio32(0x1000_0000))
                        .reg_shift(0)
                        .reg_width(0)
                        .clock_freq(3_686_400)
                        .baud_rate(115_200),
                ),
        )
        .payload(
            Payload::linux()
                .kernel_load_addr(0x8200_0000)
                .platform_fdt(0x87f0_0000)
                .opensbi_load_addr(0x8010_0000),
        )
        .build()
}
```

The “DSL” is only this pattern:

```rust
Something::new()
    .field(value)
    .field(value)
    .nested(OtherThing::new().field(value))
```

No parser-specific schema and no macro-generated target code is required.

## Typed configuration values

A major advantage over RON is that configuration values can be strongly typed:

```rust
IntelIch8::new()
    .rcba(mmio32(0xfed1_c000))
    .smbus_base(io16(0x0400));
```

This should fail at compile time:

```rust
IntelIch8::new()
    .smbus_base(mmio32(0xfed1_c000));
```

because SMBus base is an I/O port, not MMIO.

Useful newtypes:

```rust
pub struct MmioAddr<T> { raw: u64, _kind: PhantomData<T> }
pub struct IoAddr<T> { raw: u16, _kind: PhantomData<T> }
pub struct Irq(pub u8);
pub struct PciBdf { bus: u8, device: u8, function: u8 }
```

This moves many RON validation checks into normal Rust typing.

## Typed child topology and bus ports

Typed values should extend to topology, not just leaf-device configuration. A
southbridge, northbridge, SoC, or board hook often exposes several different
child attachment points. Encoding those child ports in Rust makes the board
description cleaner and prevents accidental mixes such as putting an LPC
Super I/O on a PCI-only child list.

This should be a typed builder API owned by the platform/chipset/driver crate,
not a hard-coded concept in common fstart code:

```rust
pub fn board_info() -> BoardInfo {
    I945Ich7Default::new("example-i945-board")
        .southbridge(|ich7| {
            ich7
                .pci(|pci| {
                    pci.device(
                        PciBdf::new(0, 31, 2),
                        IntelSata::new().mode(SataMode::Ahci),
                    )
                })
                .lpc(|lpc| {
                    lpc.child(
                        NsPc87392::new()
                            .config_port(io16(0x2e))
                            .com1(SerialPort::new(io16(0x3f8), Irq(4)).console()),
                    )
                })
                .smbus(|smbus| {
                    smbus.spd_eeprom(I2cAddr::new(0x50), DimmSlot::new(0))
                })
        })
        .build()
}
```

The useful invariant is that each child builder only accepts addresses and
children appropriate for that bus:

```rust
pub struct PciBus;
pub struct LpcBus;

pub struct ChildBus<'a, B> {
    parent: DeviceId,
    port: BusPortId,
    board: &'a mut Board,
    _bus: PhantomData<B>,
}

impl ChildBus<'_, PciBus> {
    pub fn device(self, bdf: PciBdf, dev: impl IntoDevice) -> Self { /* ... */ }
}

impl ChildBus<'_, LpcBus> {
    pub fn child(self, dev: impl IntoDevice) -> Self { /* ... */ }
    pub fn pnp_device(self, port: IoAddr<ConfigPort>, dev: impl IntoDevice) -> Self { /* ... */ }
}
```

The typed topology API should lower to the same generic graph used everywhere
else:

```rust
pub struct DeviceEdge {
    pub parent: DeviceId,
    pub port: BusPortId,
    pub address: Option<BusAddress>,
}

pub enum BusKind {
    Pci,
    Lpc,
    I2c,
    Smbus,
    Spi,
    SimpleBus,
}
```

For Rust-authored boards, this generic graph should not require a fake runtime
driver for every structural topology node. A `DeviceConfig` should carry a
topology role such as `Runtime`, `PciBridge`, `LpcBus`, or `SmBus`, while real
driver configs are bound by device name:

```rust
DeviceTopology::new()
    .root("northbridge")
    .root("southbridge")
    .pci_bridge("southbridge", "pcie0", 0x1c, 0, true)
    .child_bus("southbridge", "lpc", DeviceRole::LpcBus)
    .child("lpc", "superio", BusAddress::Lpc(0x2e))
    .build();

vec![
    pineview_config().bind("northbridge"),
    ich7_config().bind("southbridge"),
    superio_config().bind("superio"),
]
```

Host tooling may lower this to parallel internal tables, but that lowering is an
implementation detail. Public board/platform APIs must not make board crates pad
driver arrays with `Structural` placeholders or depend on device declaration
order for driver association.

That split is important:

- Static typed mode gets compile-time help from typed child builders.
- Dynamic board-blob mode can serialize `BusKind`, `BusPortId`, and
  `BusAddress` and then validate them at boot.
- Common flow code remains generic and traverses parent/child edges; it does not
  learn platform names like “southbridge”.

Do not require every internal bus to become a runtime driver. A child bus can be
a structural port of a parent driver when no separate lifecycle or service is
needed. Conversely, PCI host bridges, I2C controllers, or LPC bridges that need
their own setup can still appear as normal devices that expose typed child ports.

## Configuration ownership: platform defaults versus board facts

The Rust board direction should also remove a large amount of RON-era
boilerplate from board ports. Many values that appear in RON files are not
really board choices; they are fixed SoC/chipset facts or platform integration
conventions. Those should live in typed Rust defaults in platform/chipset crates,
not in every board crate.

Examples of values that should normally be hardcoded by the platform/chipset
driver or default board template:

- Intel LPC PCI BDF for a fixed southbridge generation.
- Intel RCBA base when the chipset/platform always programs the same value.
- Intel MCHBAR/DMIBAR/EPBAR defaults when fixed by the platform family.
- SMBus I/O base on platforms where firmware always reserves the same decode.
- Fixed boot ROM, SRAM, or MMIO windows from an SoC datasheet.
- Fixed interrupt controller, timer, reset, and watchdog register bases.

Examples of values that should remain board-configurable:

- UART register base when the same UART IP block appears at different addresses
  across SoCs/boards, or when a board chooses a different console UART.
- DRAM size/topology/training parameters that differ by board population.
- GPIO/pinmux choices and board straps.
- Payload paths, load addresses, boot arguments, and FIT/default configuration.
- Optional devices, board quirks, and board-specific hook devices.

The rule of thumb is: if a board author would copy the same value from a data
sheet or reference design into every port, move it into the platform/chipset
Rust code. If changing the value is a meaningful board choice, keep it in the
board crate.

Platform defaults should be represented as normal Rust constructors/builders,
not as hidden global state. A board starts from a default and overrides only the
pieces that are actually board-specific:

```rust
pub fn board_info() -> BoardInfo {
    AllwinnerA20Default::new("bananapi-m1")
        .dram(BananapiM1Dram::new().size_mib(1024))
        .console_uart(UartId::Uart0) // base address comes from the A20 default
        .payload(Payload::fit().parse_buildtime())
        .build()
}
```

The default can still expose meaningful choices without making the board spell
out raw addresses:

```rust
impl AllwinnerA20Default {
    pub fn console_uart(mut self, uart: UartId) -> Self {
        let base = match uart {
            UartId::Uart0 => 0x01c2_8000,
            UartId::Uart1 => 0x01c2_8400,
            UartId::Uart2 => 0x01c2_8800,
            UartId::Uart3 => 0x01c2_8c00,
        };

        self.board = self.board.device(
            Device::new("console")
                .driver(
                    Ns16550::new()
                        .regs(mmio32(base))
                        .reg_shift(2)
                        .clock_freq(24_000_000)
                        .baud_rate(115_200),
                ),
        );
        self
    }
}
```

For x86 platforms, defaults can capture the chipset constants while the board
only supplies Super I/O, GPIO, SPD/DRAM, and quirks:

```rust
pub fn board_info() -> BoardInfo {
    I945Ich7Default::new("example-i945-board")
        .superio(
            NsPc87392::new()
                .config_port(io16(0x2e))
                .com1(SerialPort::new(io16(0x3f8), Irq(4)).console()),
        )
        .spd_source(SmbusSpd::on_default_smbus())
        .mainboard(ExampleI945Mainboard::new().dock_console(false))
        .build()
}
```

The corresponding default template owns the boilerplate:

```rust
pub struct I945Ich7Default {
    board: Board,
}

impl I945Ich7Default {
    pub fn new(name: &'static str) -> Self {
        Self {
            board: Board::new(name)
                .platform(Platform::X86_64)
                .device(
                    Device::new("northbridge")
                        .driver(IntelI945::new().mchbar(mmio32(0xfed1_4000))),
                )
                .device(
                    Device::new("southbridge")
                        .bus(Bus::pci_bdf(PciBdf::new(0, 31, 0)))
                        .driver(
                            IntelIch7::new()
                                .rcba(mmio32(0xfed1_c000))
                                .smbus_base(io16(0x0400)),
                        ),
                ),
        }
    }

    pub fn superio(mut self, superio: NsPc87392) -> Self {
        self.board = self.board.child_of("southbridge", Device::new("superio").driver(superio));
        self
    }

    pub fn build(self) -> BoardInfo {
        self.board.build()
    }
}
```

This keeps board crates small without losing explicitness: the defaults are
reviewable Rust code, can be overridden when a platform genuinely differs, and
can still emit the same host metadata or dynamic board blob as hand-written
board definitions.

## Board crate layout and per-board build information

Each board is a board support package crate under `boards/`. A board crate exports
metadata, board facts, and recipe trait implementations. It does not contain a
committed firmware entrypoint or a committed `stage/` crate. Entrypoints are
selected by `xtask` through generated wrapper packages under `target/`.

```text
boards/
  qemu-riscv64/
    Cargo.toml
    src/lib.rs           # exports Board and recipe trait impls
    src/config.rs        # topology/runtime metadata builder
    src/build_info.rs    # host build/package metadata builder
  lenovo-x61/
    Cargo.toml
    src/lib.rs
    src/config.rs
    src/devices.rs
    src/mainboard.rs     # only for board-specific quirks/hooks
    src/smm.rs           # only when the board owns SMM behavior
```

`Cargo.toml` carries simple discovery metadata:

```toml
[package.metadata.fstart]
board = "qemu-riscv64"
platform = "riscv64"
target = "riscv64gc-unknown-none-elf"
```

`xtask build --board qemu-riscv64` scans `boards/*/Cargo.toml`, finds the
matching board metadata, obtains `BuildInfo`, and creates a selected-board
wrapper package. The Cargo metadata should stay small and stable enough for
discovery. The authoritative build information comes from normal Rust code,
using a builder parallel to `board_info()`:

```rust
pub fn board_info() -> BoardInfo {
    QemuRiscv64Default::new("qemu-riscv64")
        .memory(
            Memory::new()
                .rom("flash", 0x2000_0000, 0x0200_0000)
                .ram("ram", 0x8000_0000, 0x0800_0000),
        )
        .console_uart(UartId::Uart0)
        .payload(Payload::fit().parse_buildtime().default_config())
        .build()
}

pub fn build_info() -> BuildInfo {
    Build::new("qemu-riscv64")
        .board_package("fstart-board-qemu-riscv64")
        .target(Target::riscv64gc_unknown_none_elf())
        .profile(BuildProfile::Release)
        .stage(
            StageBuild::monolithic("stage")
                .runs_from(RunsFrom::Rom)
                .load_addr(0x2000_0000)
                .stack_size(0x40000)
                .heap_size(0x40000)
                .flow_profile(FlowProfile::LinuxBoot),
        )
        .features(
            FeatureSet::new()
                .platform(PlatformFeature::Riscv64)
                .driver::<Ns16550>()
                .driver::<PciEcam>()
                .payload_backend(PayloadBackend::Fit)
                .handoff_backend(HandoffBackend::Fdt),
        )
        .image(
            Image::raw()
                .firmware_volume(FirmwareVolume::ffs("target/ffs/qemu-riscv64.ffs"))
                .full_flash_image(false),
        )
        .payload_inputs(
            PayloadInputs::new()
                .fit("fit/qemu-riscv64.itb")
                .opensbi("fit/fw_dynamic.bin"),
        )
        .build()
}
```

The exact type names can change, but `BuildInfo` should explicitly cover the
host-side information needed to build and package a board:

| Build information | Examples | Why it is build metadata |
| --- | --- | --- |
| Board package and helper binary | package name, optional metadata emitter | Lets `xtask` build the right crate without a central registry. |
| Rust target and platform features | target triple, platform crate features, CPU family features | Selects compiler target and target-only dependencies. |
| Stage build layout | monolithic/multi-stage, load/run addresses, stack/heap, page-table/data addresses, `RunsFrom`, compression | Drives linker scripts and stage packaging. |
| Flow profile/features | early platform, memory, bus, storage, security, payload, handoff/table families | Selects handwritten flow code without encoding control flow in board data. |
| Driver/backend features | UART, PCI ECAM, ICH, FIT, FDT, ACPI, SMBIOS, SMM, boot-media providers | Selects compiled-in code and dynamic-driver registry variants. |
| Image packaging | raw/FFS/full flash, pflash size, SoC image format such as eGON, boot ROM headers | Host packaging and post-link patching. |
| Payload and firmware inputs | FIT/ELF/Linux paths, OpenSBI/TF-A blobs, microcode, initramfs, default FIT config | Host files are not target runtime topology. |
| Security packaging | signing keys or public-key inputs, manifest/hash requirements, measurement policy | Host assembly and runtime verification must agree on formats. |
| Dynamic blob policy | static typed mode vs dynamic blob, blob ABI version, registry feature set, serialization format | Determines whether board data is linked into the stage or attached later. |
| Build profile and artifact names | dev/release, output directory, final image names | Pure `xtask` policy. |

`board_info()` and `build_info()` may share helper functions, but they should not
be the same object. The board topology describes hardware/runtime facts; the
build metadata describes how `xtask` compiles, links, packages, and optionally
serializes those facts. Keeping them separate prevents host paths and Cargo
feature policy from leaking into firmware-stage runtime APIs.

For richer metadata, the board package exposes a host-buildable metadata target
that prints serialized `BoardInfo` and `BuildInfo`. Target-only runtime code is
gated behind board features so metadata extraction does not require a separate
per-board `facts/` crate. The board crate remains the single source of truth, and
there is no central `fstart-board-registry` crate.

## Fixed handwritten stage flow

The stage runner should be readable Rust code, feature-gated by build mode and
board-selected stage features. It should contain the real firmware sequence, not
just a generic `init devices` placeholder.

Be careful not to repeat the current RON/`StagePlan` problem in a different
form. Fine-grained semantic flow entries are useful when reading the stage and
when deciding where a driver participates. They should not automatically become
one Cargo feature each. Too many independent `flow-*` features make build plans
hard to reason about, produce surprising feature interactions, and force board
authors to understand internal stage plumbing.

Use a two-level model instead:

1. **Semantic flow entries** are stable named barriers in the handwritten stage
   sequence (`pre_console`, `dram`, `payload_load`, ...).
2. **Feature families / flow profiles** compile groups of entries and backend
   code (`flow-early-platform`, `flow-memory`, `flow-payload`, ...).

Suggested feature families:

| Feature family | Semantic entries it usually enables | Notes |
| --- | --- | --- |
| `flow-early-platform` | `very_early`, `early_clocks`, `pinmux`, `pre_console`, `console`, `post_console` | Most real hardware wants this as one unit; QEMU/simple boards can run mostly no-op methods. |
| `flow-memory` | `memory_discovery`, `dram`, `post_dram` | Includes fixed-RAM validation for virtual boards and real DRAM init for hardware boards. |
| `flow-bus` | `bus_early`, `bus_probe`, `drivers_ready` | Bus enumeration/resource windows without tying the flow to PCI only. |
| `flow-storage` | `storage` | Firmware volume and boot-media access; backend features choose SPI/MMC/FFS details. |
| `flow-security` | `security`, `payload_verify` | Measurement/signature policy should be explicit but not split into many stage features initially. |
| `flow-payload` | `payload_select`, `payload_load` | Backend features choose FIT/LinuxBoot/ELF/next-stage behavior. |
| `flow-handoff` | `handoff`, `boot` | Backend features choose FDT/ACPI/SMBIOS/UEFI/SMM table support. |
| `flow-multi-stage` | stage-load/next-stage helpers when not monolithic | Separate because many boards will never need it. |

Backend features such as `payload-fit`, `handoff-fdt`, `handoff-acpi`,
`soc-egon`, or `bootmedia-fel` should select implementation code. They should
not create new top-level flow entries unless they introduce a real ordering
barrier that multiple boards/drivers need.

Initial flow entries should be explicit enough to answer "where does this
happen?" when reading `fstart-stage`, while still delegating hardware-specific
work to `HardwareInit` participants:

| Flow entry | Purpose | Typical participants |
| --- | --- | --- |
| `arch_entry` | Minimal CPU/ABI setup already needed to run Rust safely. | platform crate |
| `very_early` | Board/SoC operations that must happen before clocks or console. | watchdog, reset, strap sampler, tiny SRAM setup |
| `early_clocks` | Enable clocks/resets needed by pinmux, UART, timers, and DRAM setup. | CCU/PMU/chipset clock blocks |
| `pinmux` | Route pins for early UART, DRAM, boot media, and board straps. | GPIO/pinctrl/mainboard hook |
| `pre_console` | Decode/register access required before a console driver can work. | LPC decode, Super I/O config, UART clock gate |
| `console` | Initialize selected boot console and install logging backend. | UART/Super I/O/console mux |
| `post_console` | Diagnostics and safety checks that are useful once logging works. | reset-cause logger, boot-mode reporter |
| `memory_discovery` | Read SPD/straps/board config needed for memory init. | SMBus/SPD, eFuses, board hook |
| `dram` | Train/enable DRAM or validate fixed RAM setup. | DRAM controller, PHY, northbridge |
| `post_dram` | Switch stack/heap, clear BSS if needed, enable caches/MMU regions. | arch/platform memory code |
| `bus_early` | Enable fixed bridges/resources needed before probing children. | PCI host bridge, LPC, simple-bus windows |
| `bus_probe` | Discover or instantiate child devices on enumerable buses. | PCI, USB, MMC/SD, I2C when needed |
| `storage` | Bring up boot media and firmware-volume access. | SPI flash, MMC, FFS reader |
| `security` | Measure/verify firmware volume, payloads, and policy inputs. | crypto/TPM/measurement hooks |
| `payload_select` | Choose FIT config/kernel/payload and collect handoff metadata. | payload policy, board hook |
| `payload_load` | Copy/decompress payload components to their load addresses. | FIT/LinuxBoot/ELF loaders, DMA-safe copy |
| `payload_verify` | Verify loaded image hashes/signatures if not already verified. | FIT hash/signature, measured boot |
| `handoff` | Finalize tables/FDT/boot params and quiesce firmware-owned devices. | FDT fixups, ACPI/coreboot-table-like handoff |
| `boot` | Jump to payload or next firmware stage. | arch payload launcher |

The implementation may omit semantic entries that no board uses, but new
hardware work must add a named flow entry rather than recreating an opaque
`Device::init()` bucket.

Example stage shape (feature families gate groups, not every individual
semantic step):

```rust
pub fn run<B: Board>() -> ! {
    let mut board = B::new_or_halt();

    #[cfg(feature = "flow-early-platform")]
    {
        run_step(&mut board, CapabilityEvent::VeryEarlyInit, |b, ctx| {
            b.devices_mut().very_early(ctx)
        });
        run_step(&mut board, CapabilityEvent::EarlyClockInit, |b, ctx| {
            b.devices_mut().early_clocks(ctx)
        });
        run_step(&mut board, CapabilityEvent::PinmuxInit, |b, ctx| {
            b.devices_mut().pinmux(ctx)
        });
        run_step(&mut board, CapabilityEvent::PreConsoleInit, |b, ctx| {
            b.devices_mut().pre_console(ctx)
        });
        run_step(&mut board, CapabilityEvent::ConsoleInit, |b, ctx| {
            b.devices_mut().console(ctx)?;
            b.install_console()
        });
        run_step(&mut board, CapabilityEvent::PostConsoleInit, |b, ctx| {
            b.devices_mut().post_console(ctx)
        });
    }

    #[cfg(feature = "flow-memory")]
    {
        run_step(&mut board, CapabilityEvent::MemoryDiscovery, |b, ctx| {
            b.devices_mut().memory_discovery(ctx)
        });
        run_step(&mut board, CapabilityEvent::DramInit, |b, ctx| {
            b.devices_mut().dram(ctx)
        });
        run_step(&mut board, CapabilityEvent::PostDramInit, |b, ctx| {
            b.devices_mut().post_dram(ctx)?;
            b.relocate_stack_and_heap_if_needed(ctx)
        });
    }

    #[cfg(feature = "flow-bus")]
    {
        run_step(&mut board, CapabilityEvent::BusEarlyInit, |b, ctx| {
            b.devices_mut().bus_early(ctx)
        });
        run_step(&mut board, CapabilityEvent::BusProbe, |b, ctx| {
            b.devices_mut().bus_probe(ctx)?;
            b.devices_mut().drivers_ready(ctx)
        });
    }

    #[cfg(feature = "flow-storage")]
    run_step(&mut board, CapabilityEvent::StorageInit, |b, ctx| {
        b.devices_mut().storage(ctx)?;
        b.mount_firmware_volume(ctx)
    });

    #[cfg(feature = "flow-security")]
    run_step(&mut board, CapabilityEvent::SecurityInit, |b, ctx| {
        b.devices_mut().security(ctx)?;
        b.verify_firmware_volume(ctx)
    });

    #[cfg(feature = "flow-payload")]
    {
        run_step(&mut board, CapabilityEvent::PayloadSelect, |b, ctx| {
            b.select_payload(ctx)
        });
        run_step(&mut board, CapabilityEvent::PayloadLoad, |b, ctx| {
            b.devices_mut().payload_load(ctx)?;
            b.payload_load(ctx)
        });

        #[cfg(feature = "flow-security")]
        run_step(&mut board, CapabilityEvent::PayloadVerify, |b, ctx| {
            b.verify_loaded_payload(ctx)
        });
    }

    #[cfg(feature = "flow-handoff")]
    {
        run_step(&mut board, CapabilityEvent::HandoffFinalize, |b, ctx| {
            b.devices_mut().handoff(ctx)?;
            b.finalize_handoff(ctx)
        });
        board.boot_payload_or_halt();
    }

    fstart_arch::halt()
}
```

The code flow is fixed and readable. Board-specific behavior plugs into devices
and hooks instead of generated stage glue. A small board can select a tiny set of
feature families; a PC-class board can enable more families and backends without
changing the overall model.

## Step-based hardware initialization

The lifecycle is not a single `Device::init()` method. Hardware initialization is
naturally step-based.

The trait should contain driver-participation barriers, not every operation the
stage runner performs. Add or keep a method when it represents a real ordering
point that multiple drivers or boards can participate in. Do not add a new
method merely because one payload backend needs an internal helper; that belongs
behind a backend feature inside `flow-payload` or `flow-handoff`.

Use one trait with default no-op methods:

```rust
pub trait HardwareInit {
    #[inline(always)]
    fn very_early(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), Error> {
        Ok(())
    }

    #[inline(always)]
    fn early_clocks(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), Error> {
        Ok(())
    }

    #[inline(always)]
    fn pinmux(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), Error> {
        Ok(())
    }

    #[inline(always)]
    fn pre_console(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), Error> {
        Ok(())
    }

    #[inline(always)]
    fn console(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), Error> {
        Ok(())
    }

    #[inline(always)]
    fn post_console(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), Error> {
        Ok(())
    }

    #[inline(always)]
    fn memory_discovery(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), Error> {
        Ok(())
    }

    #[inline(always)]
    fn dram(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), Error> {
        Ok(())
    }

    #[inline(always)]
    fn post_dram(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), Error> {
        Ok(())
    }

    #[inline(always)]
    fn bus_early(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), Error> {
        Ok(())
    }

    #[inline(always)]
    fn bus_probe(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), Error> {
        Ok(())
    }

    #[inline(always)]
    fn drivers_ready(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), Error> {
        Ok(())
    }

    #[inline(always)]
    fn storage(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), Error> {
        Ok(())
    }

    #[inline(always)]
    fn security(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), Error> {
        Ok(())
    }

    #[inline(always)]
    fn payload_load(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), Error> {
        Ok(())
    }

    #[inline(always)]
    fn handoff(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), Error> {
        Ok(())
    }
}
```

Drivers implement only the steps they need:

```rust
impl HardwareInit for IntelIch8 {
    fn pre_console(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), Error> {
        self.open_lpc_decode()?;
        self.enable_gpio_decode()?;
        Ok(())
    }

    fn bus_early(&mut self, ctx: &mut InitContext<'_>) -> Result<(), Error> {
        self.setup_smbus(ctx)?;
        self.enable_pci_resources()?;
        Ok(())
    }

    fn handoff(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), Error> {
        self.lockdown()
    }
}
```

This is preferred over a single `run_step(match step)` method because it is less
boilerplate and more self-documenting.

## Optimization expectation

Default no-op step methods should optimize away in static typed mode if calls are
statically dispatched through concrete types/generics.

Preferred:

```rust
pub struct Devices {
    gm965: IntelGm965,
    ich8: IntelIch8,
    superio: NsPc87392,
    mainboard: LenovoX61Mainboard,
}

impl HardwareInit for Devices {
    fn pre_console(&mut self, ctx: &mut InitContext<'_>) -> Result<(), Error> {
        self.gm965.pre_console(ctx)?;
        self.ich8.pre_console(ctx)?;
        self.superio.pre_console(ctx)?;
        self.mainboard.pre_console(ctx)?;
        Ok(())
    }

    fn dram(&mut self, ctx: &mut InitContext<'_>) -> Result<(), Error> {
        self.gm965.dram(ctx)?;
        self.ich8.dram(ctx)?;
        self.superio.dram(ctx)?;
        self.mainboard.dram(ctx)?;
        Ok(())
    }
}
```

Because `dram()` defaults to `Ok(())` for most devices and the concrete types are
known, LLVM should inline and remove those calls in release builds.

Avoid the main traversal using:

```rust
Vec<Box<dyn HardwareInit>>
&mut dyn HardwareInit
```

in static typed mode, because dynamic dispatch prevents reliable no-op
elimination.

If code size becomes a problem later, add optional step masks/associated consts,
but do not start there.

## Board-specific hooks stay in BSPs and platform recipes

Board-specific behavior is not special framework code and is not a generic
`mainboard` abstraction. It lives in the board crate and is exposed through the
selected platform recipe's board trait. The common framework does not learn names
such as `southbridge`, `northbridge`, `dock`, or `ec`.

Example platform-specific hook trait:

```rust
pub trait Gm965Ich8Mainboard {
    fn pre_console(&mut self, ich8: &mut IntelIch8) -> Result<(), Error> {
        let _ = ich8;
        Ok(())
    }

    fn handoff(&mut self, ich8: &mut IntelIch8) -> Result<(), Error> {
        let _ = ich8;
        Ok(())
    }
}
```

Example board implementation:

```rust
impl Gm965Ich8Mainboard for LenovoX61Mainboard {
    fn pre_console(&mut self, ich8: &mut IntelIch8) -> Result<(), Error> {
        self.setup_dock_console(ich8)
    }

    fn handoff(&mut self, _ich8: &mut IntelIch8) -> Result<(), Error> {
        self.quiesce_i8042_for_os();
        Ok(())
    }
}
```

A board-specific device can still implement `HardwareInit` directly when it is a
real participant in the recipe's device container. The rule is ownership: generic
framework traits stay generic, platform recipe traits stay platform-specific, and
one-off board quirks stay in the BSP.

## Device graph and traversal

Mainboards describe topology, not generic init ordering. They should normally
start from platform defaults so fixed chipset/SoC resources are not repeated in
every port:

```rust
Gm965Ich8Default::new("lenovo-x61")
    .superio(
        NsPc87392::new()
            .config_port(io16(0x2e))
            .com1(SerialPort::new(io16(0x3f8), Irq(4)).console()),
    )
    .mainboard(
        LenovoX61Mainboard::new()
            .southbridge("southbridge")
            .dock_early_console(true),
    )
    .build();
```

The expanded graph still gives parent-before-child traversal, but it can be
assembled by the default template. Drivers decide which steps they participate
in. Stage flow decides which steps run and when.

## Ordering overrides and priority numbers

The default ordering should be deterministic and boring:

1. The stage runner fixes the order of semantic flow entries.
2. Parent devices run before children for a given entry unless the entry
   explicitly documents a different traversal.
3. Typed child ports can define natural ordering for their bus, such as PCI BDF
   order or declaration order for non-enumerable LPC children.
4. Declaration order is the final stable tie-breaker.

This should cover most boards. If it does not, prefer an explicit relationship
over a magic number:

```rust
Board::new("example")
    .order(FlowStep::PreConsole, Order::before("superio", "uart0"))
    .order(FlowStep::BusEarly, Order::after("smbus", "lpc"));
```

Explicit constraints can be validated: missing devices are errors, cycles are
errors, and a constraint is scoped to one flow step. They also document the
reason a board differs from default traversal.

A numeric priority/hint may still be useful as a last resort, but it must have
clear guarantees if exposed at all:

```rust
Device::new("dock-quirk")
    .driver(LenovoDockQuirk::new())
    .order_hint(FlowStep::PreConsole, OrderHint::before_default());
```

Rules for such hints:

- They are local to one flow step and one sibling/bus group.
- They never move a child before a parent dependency.
- They never cross semantic flow boundaries.
- They are stable and deterministic: sort by dependency graph, then hint, then
  declaration order.
- Driver crates should avoid publishing magic priority values as ABI. Board and
  platform templates may use hints to resolve local quirks.

Do not provide a “best effort/no guarantees” ordering API. If firmware depends
on an order, the framework should either guarantee it or reject the configuration
as under-specified.

## Static typed mode

In static typed mode, the selected board is a concrete Rust type and the selected
platform recipe constructs concrete stage device containers. The board crate does
not own the firmware entrypoint and does not implement a per-board `StaticBoard`
adapter. `xtask` creates a temporary selected-board wrapper package that aliases
the selected BSP to a stable crate name and calls the recipe.

Example shape:

```rust
pub struct Board;

impl FirmwareBoard for Board {
    type Recipe = Gm965Ich8UefiRecipe<Self>;

    const NAME: &'static str = "lenovo-x61";
    const PLATFORM: Platform = Platform::X86_64;

    fn board_info() -> BoardInfo {
        config::board_info()
    }

    fn build_info() -> BuildInfo {
        config::build_info()
    }
}

impl Gm965Ich8UefiBoard for Board {
    type Mainboard = LenovoX61Mainboard;

    fn platform_config() -> Gm965Ich8Config {
        config::gm965_ich8_config()
    }

    fn mainboard() -> Result<Self::Mainboard, Error> {
        LenovoX61Mainboard::new(devices::mainboard_config())
    }
}
```

The selected-board wrapper is build glue, not board-authored stage flow:

```rust
use fstart_board_selected::Board;

#[no_mangle]
pub extern "C" fn fstart_main(handoff: usize) -> ! {
    fstart_stage_template::run::<Board>(handoff)
}
```

The recipe owns generic device construction and ordering. The board supplies only
facts, configs, and board-specific hooks.

## Dynamic board-blob mode

Some deployments need the board information to be added later as a binary blob,
not compiled into the stage. This is a separate build mode, not a compatibility
layer for static BSPs.

The same Rust builder can emit a serialized board blob:

```rust
Board::new("lenovo-x61")
    .platform(Platform::X86_64)
    .memory(...)
    .device(...)
    .build_blob()
```

The stage contains a compiled-in driver registry and deserializes the blob at
boot:

```rust
let blob = BoardBlob::load()?;
let mut devices = DynamicDevices::from_blob(blob, COMPILED_DRIVERS)?;
```

The blob contains runtime facts:

```rust
pub struct DeviceBlob<'a> {
    pub name: HString<32>,
    pub parent: Option<DeviceId>,
    pub bus_port: Option<BusPortId>,
    pub bus_kind: Option<BusKind>,
    pub bus_address: Option<BusAddress>,
    pub compatible: HString<64>,
    pub config_bytes: &'a [u8],
    pub enabled: bool,
}
```

Dynamic blobs cannot get the compile-time guarantees of typed child builders,
so the serialized form must carry enough topology to validate at boot: parent
ID, parent port, bus kind, and address. Runtime validation checks that, for
example, an `Lpc` address is attached to an LPC-capable port and that the chosen
driver can consume that attachment.

Drivers are matched by a stable driver ID or compatible string:

```rust
"intel,ich8" -> IntelIch8
"ns16550a"   -> Ns16550
```

A truly generic dynamic stage contains reusable drivers only. Board-specific Rust
hooks require a selected-board wrapper and are not smuggled into a central dynamic
board registry.

The dynamic runtime device can be a closed enum over compiled-in driver
features:

```rust
pub enum RuntimeDevice {
    IntelIch8(IntelIch8),
    Ns16550(Ns16550),
    Structural(StructuralDevice),
}

impl HardwareInit for RuntimeDevice {
    fn pre_console(&mut self, ctx: &mut InitContext<'_>) -> Result<(), Error> {
        match self {
            RuntimeDevice::IntelIch8(d) => d.pre_console(ctx),
            RuntimeDevice::Ns16550(d) => d.pre_console(ctx),
            RuntimeDevice::Structural(d) => d.pre_console(ctx),
        }
    }

    fn handoff(&mut self, ctx: &mut InitContext<'_>) -> Result<(), Error> {
        match self {
            RuntimeDevice::IntelIch8(d) => d.handoff(ctx),
            RuntimeDevice::Ns16550(d) => d.handoff(ctx),
            RuntimeDevice::Structural(d) => d.handoff(ctx),
        }
    }
}
```

The enum and registry boilerplate is a good candidate for an optional derive
macro because it is mechanical and easy to get wrong by hand:

```rust
#[derive(RuntimeDriverRegistry)]
#[fstart_registry(config_format = "postcard")]
pub enum RuntimeDevice {
    #[fstart_driver(
        id = "intel,ich8",
        feature = "driver-intel-ich8",
        config = IntelIch8Config,
    )]
    IntelIch8(IntelIch8),

    #[fstart_driver(
        id = "ns16550a",
        feature = "driver-ns16550",
        config = ns16550::Config,
    )]
    Ns16550(Ns16550),

    #[fstart_structural]
    Structural(StructuralDevice),
}
```

The derive could generate:

- `HardwareInit` forwarding for every step method.
- A `DriverRegistry` table of stable IDs/compatibles.
- Config deserialization and construction dispatch.
- Feature-gated variants for compiled-in drivers.
- Diagnostic strings for “driver missing from this generic stage”.

This does not violate the “no macro-heavy DSL” non-goal if the macro is limited
to registry boilerplate. Board authors should still be able to write the enum and
trait impls manually, and the blob ABI/registry traits must be documented without
depending on macro expansion.

This is not per-board generated code. It is a feature-selected driver-registry
backend.

Dynamic mode trades compile-time optimization/type checking for the ability to
ship a generic stage and attach board data later.

## Validation split

### Static typed validation

Rust types catch many issues:

- MMIO vs I/O address mixups.
- IRQ/address wrapper misuse.
- driver config field types.
- child attached to the wrong typed bus/port.
- capability helper types, where applicable.

Builder/build-time checks still handle:

- duplicate device names.
- invalid topology.
- zero or multiple selected consoles.
- unsupported platform/stage feature combinations.
- `BuildInfo`/`BoardInfo` mismatches, such as a target triple that does not
  match `Platform` or a flow profile that needs a backend feature that was not
  selected.
- missing payload files.

### Dynamic board-blob validation

Runtime validation is required because the board graph is no longer known at
compile time:

- blob ABI/version matches the stage.
- blob platform matches the compiled target.
- every compatible/driver ID has a compiled-in driver.
- config bytes deserialize for the selected driver.
- parent bus ports/kinds are compatible with child bus addresses.
- ordering constraints and local order hints are valid and acyclic.
- selected console exists.
- required payload/flash descriptors exist.

## Build metadata versus runtime flow

Avoid thinking of any builder result as generated-stage input. There are three
separate products with different consumers:

1. **`BoardInfo`**: topology, typed device configuration, memory map, payload
   policy, and optional dynamic-blob facts. It is used by validation, static
   board construction helpers, and dynamic blob serialization.
2. **`BuildInfo`**: package name, target triple, selected feature families,
   driver/backend features, linker/stage packaging, host input files, image
   formats, and artifact naming. It is used by `xtask`, linker/image assembly,
   and optional board-blob creation.
3. **Target runtime code**: fixed handwritten stage flow plus board/device
   `HardwareInit` implementations.

Neither metadata object should define runtime control flow. `BuildInfo` may
select a flow profile such as `FlowProfile::LinuxBoot`, but that profile only
chooses which handwritten flow families compile into the stage. The order and
meaning of those entries still live in the fixed stage runner.

## Required cutover state

This direction intentionally breaks the old RON/generated-stage architecture.
There is no compatibility layer and no parallel legacy path.

Required end state:

- Rust board definitions are the only board description format.
- RON board files, RON deserialization, and central RON validation are gone.
- Generated `_BoardDevices` and `StagePlan` stage glue are gone.
- `Device::init()` is gone as a primary lifecycle hook; drivers use `HardwareInit`
  step methods.
- Board crates do not contain committed firmware entrypoints or stage adapters.
- Platform recipes own reusable stage sequencing.
- `xtask` discovers board crates from `boards/*/Cargo.toml`, obtains `BuildInfo`,
  and creates selected-board wrapper packages under `target/`.
- Flow selection uses coarse flow profiles and backend features, not one feature
  per semantic step.
- Numeric ordering hints are absent unless they are deterministic, scoped, and
  validated. Undefined ordering APIs are not allowed.
- Static typed mode is the primary path. Dynamic board-blob mode is a distinct
  mode with its own registry and validation rules.
