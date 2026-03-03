# fstart Architecture

This document explains how fstart is built: how a board RON file becomes a
running firmware, how the crates fit together, and why the system is designed
the way it is.

## Core idea

A firmware build has two halves: a host-side pipeline that runs on your
workstation, and a target-side binary that runs on the board. The host side
reads the board RON file, generates Rust source code and a linker script, then
compiles that source into a bare-metal binary. The target side is that binary
— it initializes hardware and boots an OS.

The board RON file is the only input. There is no hand-written stage code, no
Makefile per board, no Kconfig. Adding a board means adding one file.

## Build pipeline

```
boards/my-board/board.ron
       |
       v
  xtask (host binary)
       |
       |  1. parse RON -> BoardConfig
       |  2. determine target triple, cargo features, env vars
       |  3. invoke: cargo build -p fstart-stage --target <triple>
       |             --features <features> -Z build-std=core
       |             (sets FSTART_BOARD_RON=<path>)
       |
       v
  fstart-stage/build.rs (runs during cargo build)
       |
       |  4. read $FSTART_BOARD_RON
       |  5. two-phase parse: RON -> RonBoardConfig -> ParsedBoard
       |  6. call stage_gen -> write generated_stage.rs to $OUT_DIR
       |  7. call linker    -> write link.ld to $OUT_DIR
       |
       v
  fstart-stage/src/main.rs
       |
       |  8. include!(generated_stage.rs)
       |  9. compile against link.ld
       |
       v
  ELF binary
       |
       |  10. llvm-objcopy -O binary (flat binary for all platforms)
       |  11. (armv7/sunxi) patch eGON header: length + checksum
       |
       v
  stage binary on disk
       |
       |  (if multi-stage or payload blobs exist)
       |  12. xtask assemble: parse ELF segments, build FFS image,
       |      sign manifest, patch anchor block
       |
       v
  firmware.ffs (or pflash image for QEMU)
       |
       |  13. xtask run: launch QEMU with the image
       v
  running firmware
```

### Step details

**Steps 1-3: xtask orchestration.** Xtask is a standard Rust xtask binary.
It locates the board RON under `boards/`, parses it to determine the
platform (`riscv64` / `aarch64` / `armv7`), maps that to a target triple
(`riscv64gc-unknown-none-elf`, etc.), and derives cargo feature flags from
the board config. Features come from two sources: the drivers listed in
`devices` (e.g. a board with an `Ns16550` device enables the `ns16550`
feature), and the capabilities listed in each stage (e.g. `SigVerify`
enables `ed25519` and digest features). Xtask then invokes cargo with
`-Z build-std=core` (or `core,alloc` when a heap is needed).

**Steps 4-7: build.rs codegen.** The `fstart-stage` crate has a build script
that reads the board RON from the `FSTART_BOARD_RON` environment variable.
It performs a two-phase parse: first into a `RonBoardConfig` (generic RON
structure), then into a `ParsedBoard` with typed driver configs via the
device registry. The parsed board is passed to two generators:

- `stage_gen::generate_stage_source()` produces a complete Rust source file
  containing the `fstart_main()` function, device structs, and all
  initialization logic.
- `linker::generate_linker_script()` produces a `link.ld` with memory
  regions derived from the board's memory map.

The build script emits `cargo:rustc-link-arg=-Tlink.ld` so the linker
picks up the generated script automatically.

**Steps 8-9: compilation.** `fstart-stage/src/main.rs` is four lines:

```rust
#![no_std]
#![no_main]
include!(concat!(env!("OUT_DIR"), "/generated_stage.rs"));
```

Everything else comes from codegen. The platform crate provides `_start`
(the entry point), and `fstart-runtime` provides the `#[panic_handler]`.

**Steps 10-11: post-processing.** All platforms produce flat binaries via
`llvm-objcopy -O binary`. The `.bss` section is stripped to avoid a
multi-gigabyte file spanning the ROM-to-RAM address gap. For Allwinner SoCs,
the binary is further patched with an eGON boot header (magic bytes, length,
checksum) that the SoC's BROM expects.

**Steps 12-13: assembly and launch.** For multi-stage boards or boards with
payload blobs (kernel, OpenSBI, ATF), xtask runs the assembly step: it
parses the ELF to extract segments, packages everything into an FFS image,
signs the manifest, and patches the anchor block into the bootblock binary.
QEMU is launched with the resulting image — either via pflash (RISC-V) or
`-bios` (AArch64/ARMv7).

## Crate graph

fstart has 26 crates in three groups: host-only, shared, and target-only.

### Host-only (std)

```
xtask
  reads board RON, invokes cargo, assembles FFS, launches QEMU
  depends on: fstart-types, fstart-ffs, fstart-fit, fstart-crypto,
              fstart-device-registry, goblin (ELF parsing),
              ed25519-dalek (signing)

fstart-codegen
  RON parser, Rust code generator, linker script generator
  used by: fstart-stage/build.rs
  depends on: fstart-types, fstart-device-registry, syn, quote,
              prettyplease, proc-macro2

fstart-device-registry
  maps DriverInstance variants to driver crate metadata
  depends on: every fstart-driver-* crate (feature-gated)
```

### Shared (std for host, no_std for target)

```
fstart-types
  BoardConfig, MemoryMap, DeviceConfig, StageLayout, Capability,
  PayloadConfig, SecurityConfig, FFS types (AnchorBlock, Manifest, etc.)
  used by: everything

fstart-ffs
  firmware filesystem reader (no_std) + builder (std)
  depends on: fstart-types, fstart-crypto, postcard (serde)

fstart-fit
  FIT image parser (U-Boot .itb format)
  depends on: fstart-crypto, dtoolkit (FDT parser)
```

### Target-only (no_std, no_main)

```
fstart-stage           the final binary; include!s generated code
fstart-runtime         #[panic_handler]
fstart-services        trait definitions (Console, BlockDevice, Device, ...)
fstart-capabilities    capability implementations called from generated code
fstart-log             global logger backed by ufmt
fstart-arch            architecture helpers (delay loops, halt)
fstart-mmio            MMIO register access
fstart-crypto          signature verification, hashing
fstart-alloc           bump allocator

fstart-platform-*      _start entry, stack setup, boot protocol jumps
fstart-soc-sunxi       Allwinner eGON header, FEL support

fstart-driver-*        individual hardware drivers
```

### Dependency flow

```
board.ron ──► fstart-codegen ──► generated_stage.rs
                  |                     |
                  v                     v
          fstart-device-registry   fstart-stage
                  |                     |
                  v                     v
          fstart-driver-*          fstart-services
                                        |
                                        v
                                   fstart-capabilities
                                        |
                                        v
                                   fstart-ffs, fstart-crypto, fstart-log
```

Host tools (xtask, codegen) depend on everything. Target crates have a
strict layered dependency: `fstart-stage` depends on `fstart-capabilities`,
which depends on `fstart-ffs` and `fstart-crypto`, which depend on
`fstart-types`. Driver crates depend only on `fstart-services` (for the
traits they implement) and register-access crates.

## Code generation

The code generator (`fstart-codegen/src/stage_gen/`) transforms a
`ParsedBoard` into a formatted Rust source file. The generator is organized
into submodules:

```
stage_gen/
  mod.rs          top-level generate_stage_source()
  capabilities.rs code emitters for each Capability variant
  config_ser.rs   serializes driver configs into Rust struct literals
  flexible.rs     generates service dispatch enums (Flexible mode)
  registry.rs     maps driver metadata to code generation
  tokens.rs       helper functions (halt expressions, hex literals)
  topology.rs     device tree validation (cycles, missing parents)
  validation.rs   capability ordering checks
```

### What gets generated

`generate_stage_source()` builds a `proc_macro2::TokenStream` by calling
generators in sequence. The output has these sections:

1. `extern crate` declarations for the platform and runtime crates.
2. `use` imports for driver types, service traits, and capabilities.
3. Constants (`FLASH_BASE`, `FLASH_SIZE`) when boot media is memory-mapped.
4. Allwinner eGON header (global_asm + static) for sunxi first stages.
5. FFS anchor static in `.fstart.anchor` — a placeholder patched after build.
6. Heap storage (a large aligned static) when `heap_size` is set.
7. Service dispatch enums (Flexible mode only).
8. `Devices` struct with one field per device.
9. `StageContext` struct with typed service accessors.
10. `DEVICE_TREE` static (flat table of `DeviceNode` entries).
11. `fstart_main()` — the entry point with the full init sequence.

The token stream is parsed into a `syn::File` AST and formatted with
`prettyplease` to produce readable Rust.

### fstart_main() structure

The generated `fstart_main(handoff_ptr: usize) -> !` has three phases:

**Device construction.** Every device is constructed with its exact config:

```rust
let uart0 = Ns16550::new(&Ns16550Config {
    base_addr: 0x10000000,
    clock_freq: 3686400,
    baud_rate: 115200,
    reg_shift: 0,
}).unwrap_or_else(|_| halt!());
```

Bus-attached devices receive their parent by name:

```rust
let sensor = Bmp280::new_on_bus(&Bmp280Config { addr: 0x76 }, &i2c0)
    .unwrap_or_else(|_| halt!());
```

All parent references are resolved at compile time — no runtime device
lookup.

**Capability execution.** Each capability in the stage's list maps to
a block of generated code. The order in the RON file is the execution order.
For example, `ConsoleInit { device: "uart0" }` generates:

```rust
uart0.init().unwrap_or_else(|_| halt!());
unsafe { fstart_log::init(&uart0) };
fstart_capabilities::console_ready("uart0", "ns16550");
```

An `inited_devices` set tracks which devices have already been initialized
to prevent double-init when `DriverInit` runs later.

**Finalize.** Constructs the `StageContext`, logs completion, and halts. If
the last capability was a jump (`PayloadLoad`, `StageLoad`), the halt is
unreachable — it exists as a safety backstop.

### Rigid vs Flexible mode

In **Rigid** mode, every field in `Devices` has the concrete driver type.
Service accessors return `&impl Console` (or whichever trait). The compiler
sees through everything and can inline, DCE, and optimize aggressively.
One board, one binary, zero overhead.

In **Flexible** mode, the generator emits a dispatch enum for each service
that has multiple possible drivers:

```rust
enum ConsoleDevice { Uart0(Ns16550) }
impl Console for ConsoleDevice {
    fn write_byte(&self, byte: u8) -> Result<(), ServiceError> {
        match self { ConsoleDevice::Uart0(d) => d.write_byte(byte) }
    }
}
```

This avoids trait objects and heap allocation while allowing a single binary
to support multiple hardware variants at runtime.

### Validation

Before generating code, the pipeline validates:

- **Capability ordering**: `ConsoleInit` must appear before capabilities that
  log. `BootMedia` must appear before `SigVerify`, `StageLoad`, or
  `PayloadLoad`. `ClockInit` must appear before `ConsoleInit` when both exist.
- **Device references**: Every device named in a capability must exist in the
  `devices` list.
- **Device tree topology**: No cycles, no missing parents, bus children
  reference valid bus controllers.

Validation failures produce `compile_error!()` in the generated source,
surfacing the problem through the normal cargo error reporting.

### Linker script generation

The linker script generator reads the board's memory map and stage config to
produce a layout appropriate for the boot mode:

**XIP (code in ROM, data in RAM):** Two `MEMORY` regions. `.text` and
`.rodata` go into `ROM`. `.data` has its VMA in `RAM` but loads from `ROM`
via an `AT > ROM` directive — the `_start` assembly copies it at boot.
`.bss` is `NOLOAD` in `RAM`. Stack grows down from the top of `RAM`.

**RAM-only:** A single `MEMORY` region starting at `load_addr`. Everything
is contiguous. `.data` does not need copying (LMA == VMA), and `_start`
skips the copy loop when it detects `_data_load == _data_start`.

For Allwinner eGON, the script places a `.head` section before `.text` so
the eGON magic appears at offset 0 of the binary.

## Runtime boot flow

### Platform entry (_start)

Each platform crate (`fstart-platform-riscv64`, etc.) provides a `_start`
in a `global_asm!` block placed in `.text.entry`. The assembly does four
things:

1. **Save boot arguments.** On RISC-V, the DTB address arrives in `a1` and
   is saved to `mscratch` (a CSR immune to stack corruption). On AArch64,
   it is saved to a dedicated register or memory location.

2. **Set up the stack.** `sp = _stack_top` (symbol from the linker script).

3. **Copy .data from ROM to RAM.** A word-by-word loop from `_data_load` to
   `_data_start..._data_end`. Skipped when source equals destination (RAM
   layout).

4. **Zero .bss.** A word-by-word loop from `_bss_start` to `_bss_end`.

Then it calls `fstart_main(0)` — the codegen-produced function.

### Capability sequence

Inside `fstart_main()`, capabilities execute in declared order. A typical
monolithic boot:

1. `ConsoleInit` — init UART hardware, register as global logger. All
   subsequent code can call `info!()`, `error!()`, etc.
2. `MemoryInit` (or `DramInit`) — initialize DRAM. On real hardware this
   runs the full training sequence; on QEMU it is a no-op.
3. `BootMedia` — declare where the FFS image lives (flash address or block
   device).
4. `SigVerify` — read the FFS anchor (volatile, to see post-build patched
   values), verify the manifest signature, verify per-file digests.
5. `FdtPrepare` — copy the platform DTB to the payload's expected address,
   patch `/chosen/bootargs` and `/memory`.
6. `PayloadLoad` — load kernel and firmware blobs from FFS to their load
   addresses, then jump via the platform boot protocol. Does not return.

For multi-stage boards, the bootblock ends with `StageLoad` instead of
`PayloadLoad`. The next stage's `fstart_main` receives a `handoff_ptr` with
DRAM size and boot media info from the previous stage.

### Platform boot protocols

Each platform crate provides a jump function:

- **RISC-V**: `boot_linux_sbi(fw_addr, hart_id, dtb_addr, &fw_dynamic_info)`
  jumps to OpenSBI with the fw_dynamic protocol. OpenSBI then starts the
  kernel in S-mode.
- **AArch64**: `boot_linux_atf(fw_addr, &bl_params)` jumps to ARM Trusted
  Firmware BL31, which starts the kernel in EL1.
- **ARMv7**: `boot_linux(kernel_addr, dtb_addr)` cleans caches and jumps
  directly to a zImage.

## Firmware filesystem (FFS)

FFS is the on-disk format for assembled firmware images. It packages stage
binaries, payload blobs, a manifest, and cryptographic signatures into a
single image.

### Layout

```
+---------------------------+
| Region 0 data             |  stage binaries, payload blobs
|   File 0 (bootblock)      |    segments: code, rodata, rwdata
|   File 1 (main stage)     |
|   File 2 (kernel)         |
|   File 3 (firmware blob)  |
+---------------------------+
| Signed manifest           |  postcard-serialized, Ed25519-signed
+---------------------------+
| Anchor block              |  embedded in bootblock via link section
|   (patched post-build)    |
+---------------------------+
```

### Anchor block

The anchor is a fixed-size `#[repr(C)]` struct embedded in the bootblock
binary at link time (in the `.fstart.anchor` section). At build time, the
FFS builder scans the image for the anchor's magic bytes and patches it
with the manifest offset, image size, and verification keys. The anchor
must be read with volatile reads at runtime because the compiler saw the
pre-patch placeholder values at compile time.

### Manifest

The manifest is a `postcard`-serialized structure listing every region, file,
and segment in the image. Each file entry includes:

- Segment list: name, kind (code/rodata/rwdata/bss), offset, sizes, load
  address, compression method.
- Digest set: optional SHA-256 and SHA3-256 hashes of the file's data.

The manifest itself is signed with Ed25519 (or ECDSA P-256). The signature
covers the serialized manifest bytes.

### Builder

The FFS builder (std-only, used by xtask) works in four phases:

1. **Layout**: assign offsets to all segments (8-byte aligned). Attempt LZ4
   compression; fall back to uncompressed if it does not save space.
2. **Sign**: serialize the manifest with postcard, sign with the provided
   closure, append the signed manifest to the image.
3. **Anchor patch**: find the placeholder anchor in the bootblock region and
   overwrite it with real offsets and keys.
4. **Re-sign**: patching the anchor invalidated the bootblock's digest.
   Recompute it from the actual image bytes and re-sign the manifest. The
   re-serialized manifest is guaranteed to be the same size.

### Reader

The FFS reader is `no_std` and zero-alloc. It borrows the firmware image as
`&[u8]` and provides:

- Manifest reading and signature verification.
- Region/file/segment lookup by name or type.
- Digest verification for individual files.
- Segment data access with three-level offset resolution
  (region + file + segment).

For memory-mapped flash, reads are zero-copy pointer arithmetic. For block
devices, reads go through a `BootMedia` trait that abstracts the storage.

### LZ4 compression

Segments can be LZ4-compressed. Decompression uses an in-place technique
following coreboot's cbfstool approach: the compressed data is read to the
tail of the destination buffer, then decompressed from tail to head. The
builder verifies at build time that the in-place operation is safe (the
read cursor never catches the write cursor). This avoids needing a separate
scratch buffer.

## Boot media abstraction

The `BootMedia` trait provides a uniform read interface over different
storage types:

```
BootMedia
  |
  +-- MemoryMapped<F: FlashMap>    zero-copy reads from memory-mapped flash
  |     read_at() -> ptr::copy
  |     as_slice() -> &[u8] directly from flash
  |
  +-- BlockDeviceMedia<B>          reads via BlockDevice trait (MMC, SPI)
  |     read_at() -> block device read
  |     as_slice() -> None (no zero-copy)
  |
  +-- SubRegion<M>                 windowed view into another BootMedia
        offset translation on reads
```

`MemoryMapped` is generic over a `FlashMap` trait that translates flash
offsets to CPU-visible addresses. The default `LinearMap` handles the common
case of contiguous mapping. The `FlashMap` abstraction exists to support SoCs
with banked or non-contiguous flash mappings.

All FFS operations are generic over `BootMedia`. For memory-mapped flash,
the compiler monomorphizes everything down to pointer arithmetic — no
function pointers, no vtable, no overhead.

## Driver / device / service architecture

### Three layers

```
Service traits (Console, BlockDevice, Timer, ...)
     ^
     |  implemented by
     |
Driver structs (Ns16550, Pl011, SunxiA20Mmc, ...)
     ^
     |  constructed via
     |
Device trait (new + init lifecycle)
```

**Service traits** define what a device can do. They live in
`fstart-services` and require `Send + Sync`. Each trait is a minimal
interface — `Console` has `write_byte` and `read_byte`, `BlockDevice` has
`read`, `write`, `size`.

**Driver structs** live in their own crates (`fstart-driver-ns16550`, etc.).
Each struct holds MMIO register references or base addresses, plus config
values needed at runtime (clock frequency, baud rate). Drivers implement
one or more service traits.

**The `Device` trait** defines the construction lifecycle:

- `type Config` — the driver's configuration struct (what goes in the RON
  file).
- `fn new(config: &Config) -> Result<Self, DeviceError>` — construct the
  driver. Stores config values but does not touch hardware.
- `fn init(&self) -> Result<(), DeviceError>` — initialize hardware.
  Programs registers, enables clocks, runs calibration.

The split between `new` (pure) and `init` (side-effectful) matters because
codegen constructs all devices first, then initializes them in capability
order.

### Bus devices

Devices on a bus (I2C sensors, SPI flash) implement `BusDevice` instead of
`Device`:

```rust
trait BusDevice {
    type Config;
    type Bus: ?Sized;
    fn new_on_bus(config: &Config, bus: &Bus) -> Result<Self, DeviceError>;
    fn init(&self) -> Result<(), DeviceError>;
}
```

The bus reference (`&Bus`) is the parent device, passed by codegen at
construction time. Bus resolution is purely compile-time — no device tree
walks, no linked lists.

### Device registry

The device registry (`fstart-device-registry`) maps RON driver variant names
to driver crate metadata. It defines a `DriverInstance` enum with one variant
per supported driver, and a `DriverMeta` struct with:

- The Rust type name (`"Ns16550"`).
- The crate path (`"fstart_driver_ns16550"`).
- The config type name (`"Ns16550Config"`).
- The list of service traits the driver implements.

Codegen uses this metadata to emit the correct `use` statements, struct
construction calls, and service accessor methods. Adding a new driver
requires adding a variant to `DriverInstance` and a feature flag — the rest
of the pipeline adapts automatically.

## Logging

`fstart-log` provides `info!()`, `error!()`, `warn!()`, `debug!()`, and
`trace!()` macros backed by `ufmt` (a code-size-efficient formatting library
for `no_std`).

A global `&'static dyn Console` reference is set once by `ConsoleInit` via
`fstart_log::init()`. The reference lifetime is extended with `transmute`
— safe because the console lives in `fstart_main()`, which never returns.

The `ConsoleWriter` translates `\n` to `\r\n` on the fly (serial terminals
require carriage returns). Before `init()` is called, log output is silently
discarded.

## Security model

Every assembled firmware image is signed and verified at boot.

**Build time:** xtask generates Ed25519 dev keys if none exist, computes
SHA-256 (and optionally SHA3-256) digests for every file in the image, signs
the manifest, and embeds the public key in the anchor block.

**Boot time:** the `SigVerify` capability reads the anchor (volatile),
deserializes the manifest, verifies the signature against the embedded
public key, then verifies per-file digests against the actual image data.
If any check fails, the firmware halts.

The anchor is read with volatile because the compiler saw the pre-patch
placeholder at compile time and would otherwise constant-fold the zeros.

## FIT image support

fstart can boot FIT images (U-Boot's `.itb` format) containing kernel,
ramdisk, FDT, and firmware in a single DTB-format blob. The `fstart-fit`
crate parses FIT images identically at build time (std) and runtime
(no_std).

**Buildtime parse:** xtask reads the `.itb`, extracts each component, and
stores them as separate FFS file entries with load addresses from the FIT
metadata. The firmware loads them the same way it loads a LinuxBoot payload.

**Runtime parse:** the entire `.itb` is embedded as a single FFS entry. At
boot, the firmware parses the FIT in-place (zero-copy on memory-mapped
flash) and copies each component to its load address.

## Multi-stage boot

Boards can define multiple stages (e.g. bootblock + main). Each stage is a
separate invocation of the codegen pipeline and produces its own
`fstart_main()`. Stages are packaged as separate files in the FFS image.

The bootblock loads the next stage via `StageLoad`: it finds the named stage
in the FFS manifest, copies its segments to the load addresses, and jumps.
The next stage receives a `handoff_ptr` with information from the previous
stage (DRAM size, boot media identity).

Codegen performs cross-stage analysis at compile time: it examines all
previous stages' capabilities to determine which devices have persistent
hardware state (clocks, DRAM) and should not be re-initialized. Devices
whose state is lost between stages (UART FIFO, MMC controller) are
re-initialized in the new stage.
