# fstart User Guide

fstart builds firmware from a board description file. You write a `.ron` file
that describes the hardware — memory layout, devices, and what the firmware
should do — and fstart generates everything else: the stage entry point, driver
initialization sequence, and linker script.

## How it works

```
boards/my-board/board.ron
  ──► codegen reads the RON file during cargo build
  ──► generates fstart_main() with driver init and boot sequence
  ──► generates link.ld from memory regions
  ──► fstart-stage compiles: _start → fstart_main() → jumps to OS
  ──► xtask assemble: stage binary + payload blobs → signed .ffs image
```

The board file is the only file you write. There is no stage source code to
maintain.

## Board file structure

Board files live at `boards/<board-name>/board.ron`. A board file is a RON
tuple with these top-level fields:

```
name             string          board identifier
platform         string          "riscv64" | "aarch64" | "armv7"
memory           MemoryMap       ROM and RAM regions
devices          [DeviceConfig]  hardware devices and their drivers
stages           StageLayout     boot stage(s) and their capability sequences
security         SecurityConfig  signing key and digest algorithms
mode             BuildMode       Rigid | Flexible
payload          PayloadConfig?  what to boot (Linux, FIT image, etc.)
soc_image_format SocImageFormat  (optional) AllwinnerEgon for Allwinner SoCs
```

### Platform

`platform` selects the target architecture and determines which `_start`
implementation is used:

- `"riscv64"` — RISC-V 64-bit; boots Linux via SBI (OpenSBI or RustSBI)
- `"aarch64"` — AArch64; boots Linux via ARM Trusted Firmware (BL31)
- `"armv7"` — ARMv7 32-bit; boots Linux directly (zImage + ATAGs/DTB)

### Memory map

`memory` declares the physical address ranges for ROM and RAM. Device MMIO
addresses go in each device's driver config, not here.

```ron
memory: (
    regions: [
        ( name: "flash", base: 0x20000000, size: 0x02000000, kind: Rom ),
        ( name: "ram",   base: 0x80000000, size: 0x08000000, kind: Ram ),
    ],
    flash_base: Some(0x20000000),  // where the FFS firmware image starts
    flash_size: Some(0x02000000),  // total size of the flash image
),
```

`flash_base` and `flash_size` define the window that fstart will read as a
firmware filesystem. Set them to `None` for boards where flash is not
CPU-addressable (e.g. eMMC-only boards).

Region `kind` values:
- `Rom` — read-only flash (XIP or memory-mapped)
- `Ram` — writable memory (DRAM, SRAM)
- `Reserved` — present but not used by fstart

### Devices

`devices` is an ordered list of hardware devices. Each entry names a device,
specifies its driver with configuration, and declares which services it
provides.

```ron
devices: [
    (
        name: "uart0",
        driver: Ns16550((
            base_addr:  0x10000000,
            clock_freq: 3686400,
            baud_rate:  115200,
        )),
        services: ["Console"],
    ),
],
```

The `name` is how you refer to this device from capabilities (e.g.
`ConsoleInit( device: "uart0" )`). The `driver` variant determines which
driver crate is used. `services` declares the service traits the device
provides; this affects flexible-mode codegen and documentation.

#### Available drivers

**NS16550 UART** (`Ns16550`)

Compatible with NS16550(A) and DesignWare APB UART (sunxi, etc.).

```ron
driver: Ns16550((
    base_addr:  u64,   // MMIO base address
    clock_freq: u32,   // input clock frequency in Hz
    baud_rate:  u32,   // target baud rate (e.g. 115200)
    reg_shift:  u8,    // register stride: 0 = 1-byte, 2 = 4-byte (sunxi/DW APB)
))
```

`reg_shift: 2` is needed for Allwinner and other SoCs that map each register
at a 4-byte stride rather than 1-byte. Omit `reg_shift` to default to `0`.

**PL011 UART** (`Pl011`)

ARM PrimeCells PL011. Used on QEMU virt AArch64/ARMv7 and most ARM boards.

```ron
driver: Pl011((
    base_addr:  u64,
    clock_freq: u32,
    baud_rate:  u32,
))
```

**DesignWare I2C** (`DesignwareI2c`)

DesignWare APB I2C controller. Provides the `I2cBus` service.

```ron
driver: DesignwareI2c((
    base_addr:  u64,
    clock_freq: u32,  // peripheral clock in Hz
    bus_speed:  u32,  // 100000 or 400000
))
```

**Allwinner A20 CCU** (`SunxiA20Ccu`)

Clock and reset controller for Allwinner A20/sun7i. Must be initialized
before UART and DRAM on sunxi boards. Provides the `ClockController` service.

```ron
driver: SunxiA20Ccu((
    ccu_base:   u64,  // CCU MMIO base (0x01C20000 on A20)
    pio_base:   u64,  // GPIO MMIO base (0x01C20800 on A20)
    uart_index: u32,  // which UART to configure (0 = UART0)
))
```

**Allwinner A20 DRAM controller** (`SunxiA20Dramc`)

Performs full DRAM initialization and calibration. Provides the
`MemoryController` service. The timing parameters are SoC and DRAM-module
specific; refer to the BananaPi M1 board file for a working A20 example.

```ron
driver: SunxiA20Dramc((
    dramc_base: u64,
    ccu_base:   u64,
    clock:      u32,   // DRAM clock in MHz (e.g. 432)
    mbus_clock: u32,
    zq:         u32,   // calibration value
    odt_en:     bool,
    cas:        u32,
    tpr0:       u32,   // timing parameter registers
    tpr1:       u32,
    tpr2:       u32,
    // additional timing fields ...
))
```

**Allwinner A20 SD/MMC** (`SunxiA20Mmc`)

SD/MMC host controller. Provides the `BlockDevice` service.

```ron
driver: SunxiA20Mmc((
    base_addr: u64,   // MMC controller base (e.g. 0x01C0F000 for MMC0)
    ccu_base:  u64,
    pio_base:  u64,
    mmc_index: u32,   // 0 = MMC0, 1 = MMC1, etc.
))
```

**Allwinner A20 SPI** (`SunxiA20Spi`)

SPI NOR flash controller. Provides the `BlockDevice` service.

```ron
driver: SunxiA20Spi((
    base_addr:  u64,
    ccu_base:   u64,
    pio_base:   u64,
    flash_size: u64,  // total flash size in bytes
))
```

### Stages

`stages` defines the boot stage layout. A stage is a self-contained binary
with an ordered list of capabilities — things the firmware does in sequence.

#### Monolithic

A single stage binary that does everything from reset to payload handoff.
Suitable for QEMU and simple boards.

```ron
stages: Monolithic((
    capabilities: [ /* ordered list */ ],
    load_addr:  u64,            // where the binary runs
    stack_size: u32,            // stack in bytes
    heap_size:  Option<u32>,    // heap in bytes (required for FdtPrepare)
    data_addr:  Option<u64>,    // RAM address for .data/.bss when running XIP
                                // needed if QEMU or BROM places data at RAM base
)),
```

`data_addr` separates the RAM region used for writable data from the address
where the code executes. Set it when the platform puts something else
(e.g. a DTB) at the start of RAM.

#### MultiStage

Two or more stages. The first stage (bootblock) typically runs from ROM and
loads the next stage into RAM. Each stage has the same fields as Monolithic,
plus a `name` and `runs_from` field.

```ron
stages: MultiStage([
    (
        name: "bootblock",
        capabilities: [
            ConsoleInit( device: "uart0" ),
            BootMedia(MemoryMapped( base: 0x20000000, size: 0x02000000 )),
            SigVerify,
            StageLoad( next_stage: "main" ),
        ],
        load_addr: 0x20000000,
        stack_size: 0x4000,
        runs_from: Rom,
    ),
    (
        name: "main",
        capabilities: [
            ConsoleInit( device: "uart0" ),
            MemoryInit,
            DriverInit,
        ],
        load_addr: 0x80100000,
        stack_size: 0x10000,
        runs_from: Ram,
    ),
]),
```

`runs_from: Rom` means the stage executes in place from flash. Writable data
and stack go to `data_addr` (RAM). `runs_from: Ram` means the stage is copied
into RAM before executing.

#### Capabilities

Capabilities are the steps a stage performs, in order. fstart generates code
for each one.

| Capability | Description |
|---|---|
| `ConsoleInit { device: "name" }` | Initialize the named UART and register it as the global logger. Must come before any logging. |
| `ClockInit { device: "name" }` | Initialize a clock controller (CCU). On sunxi, must come before `ConsoleInit`. |
| `MemoryInit` | DRAM initialization stub. No-op on QEMU; use `DramInit` for real hardware. |
| `DramInit { device: "name" }` | Run full DRAM training using the named `MemoryController` device. Logs detected size. Halts on failure. |
| `DriverInit` | Initialize all devices not already initialized by a targeted capability. |
| `LateDriverInit` | Post-OS-prep lockdown stub. Run after `FdtPrepare`, before `PayloadLoad`. |
| `BootMedia(medium)` | Declare the boot medium that FFS operations read from (see below). |
| `SigVerify` | Verify the Ed25519 manifest signature and per-file digests of the FFS image. |
| `FdtPrepare` | Copy the platform DTB to `dtb_addr`, patch `/chosen/bootargs`, and update `/memory`. Requires `heap_size`. |
| `PayloadLoad` | Load the kernel and firmware blobs from FFS and jump to the OS. Final step; does not return. |
| `StageLoad { next_stage: "name" }` | Load the named next stage from FFS and jump to it. |
| `LoadNextStage { devices: [...], next_stage: "name" }` | Raw block-device stage load (no FFS). Used for sunxi bootblocks running from SRAM. |
| `ReturnToFel` | Return to Allwinner FEL USB mode. ARMv7/sunxi only. |

#### Boot media

`BootMedia(medium)` declares where FFS reads come from. Required before
`SigVerify`, `StageLoad`, and `PayloadLoad`.

```ron
// Flash mapped directly into the CPU address space (XIP)
BootMedia(MemoryMapped( base: 0x20000000, size: 0x02000000 ))

// A block device from the devices list
BootMedia(Device( name: "mmc0", offset: 0x2000, size: 0x800000 ))

// Allwinner auto-detect: picks whichever block device the BROM booted from
BootMedia(AutoDevice(
    devices: [
        ( name: "mmc0", offset: 0x2000, size: 0x800000 ),
        ( name: "spi0", offset: 0,      size: 0x400000 ),
    ],
))
```

### Security

fstart signs the firmware image and verifies the signature at boot.

```ron
security: (
    signing_algorithm: Ed25519,         // Ed25519 or EcdsaP256
    pubkey_file: "keys/dev-signing.pub", // path relative to the board directory
    required_digests: [Sha256, Sha3_256],
),
```

The public key file is embedded in the firmware binary. The private key is
used only by `xtask assemble` to sign the image. Key generation is outside
fstart's scope; any standard Ed25519 or P-256 key pair works.

### Mode

```ron
mode: Rigid,    // Concrete types, maximum dead-code elimination (recommended)
mode: Flexible, // Enum wrappers around service traits, runtime dispatch
```

`Rigid` mode generates code where every driver type is known at compile time.
The compiler can inline, optimize, and eliminate dead code aggressively. Use
this for production builds.

`Flexible` mode generates an enum wrapper for each service type. This allows
writing board-agnostic code that dispatches at runtime, at the cost of a small
match overhead. Useful when a single binary must support multiple hardware
variants.

### Payload

`payload` describes what the firmware hands off to after completing its
capability sequence. Set to `None` for bare-metal firmware that does not boot
an OS.

#### Linux boot

```ron
payload: Some((
    kind: LinuxBoot,
    kernel_file: Some("Image"),          // filename of the kernel blob in FFS
    kernel_load_addr: Some(0x41000000),  // RAM address to load the kernel to
    fdt: Platform,                       // use the DTB passed in by the platform
    dtb_addr: Some(0x40100000),          // where to place the patched DTB
    src_dtb_addr: Some(0x40000000),      // where the platform left the DTB
                                         // (omit if the platform provides it
                                         //  in a register, e.g. RISC-V a1)
    bootargs: Some("console=ttyAMA0"),   // written to /chosen/bootargs
    firmware: Some((
        kind: ArmTrustedFirmware,        // OpenSbi | ArmTrustedFirmware
        file: "bl31.bin",
        load_addr: 0x0E090000,
    )),
)),
```

`fdt` sources:
- `Platform` — use the DTB passed by the platform firmware or QEMU
- `Override("path/to/file.dtb")` — embed a specific DTB file in the FFS image

`firmware` is required for AArch64 (ATF BL31) and RISC-V (OpenSBI/RustSBI).
ARMv7 boots Linux directly and does not need a firmware blob.

#### FIT image

```ron
payload: Some((
    kind: FitImage,
    fit_file: Some("../../fit/qemu-riscv64.itb"),
    fit_config: None,            // None = use default FIT config
    fit_parse: Some(Buildtime),  // Buildtime | Runtime
    fdt: Platform,
    dtb_addr: Some(0x87F00000),
    bootargs: Some("console=ttyS0"),
    firmware: Some(( kind: OpenSbi, file: "fw_dynamic.bin", load_addr: 0x80100000 )),
)),
```

`fit_parse: Buildtime` — xtask extracts kernel, ramdisk, and DTB from the
`.itb` at assembly time and stores them as separate FFS entries. Simpler
runtime; requires xtask to have the `.itb` at build time.

`fit_parse: Runtime` — the entire `.itb` is embedded in FFS. The firmware
parses it at boot and copies each component to its load address.

### SoC image format

```ron
soc_image_format: AllwinnerEgon,
```

When set, xtask prepends the eGON.BT0 header and patches the binary length
and checksum fields after linking. Required for Allwinner SoCs that use the
BROM boot protocol. Omit this field for all other platforms.

## Build commands

```bash
# Compile the stage binary (no QEMU, no FFS image)
cargo xtask build --board <name>
cargo xtask build --board <name> --release

# Build and launch in QEMU (assembles FFS image if needed)
cargo xtask run --board <name>
cargo xtask run --board <name> --kernel path/to/Image --firmware path/to/fw.bin

# Build and package a signed FFS firmware image → target/ffs/<name>.ffs
cargo xtask assemble --board <name>
cargo xtask assemble --board <name> --kernel path/to/Image

# Inspect the contents of a built FFS image
cargo xtask inspect --image target/ffs/<name>.ffs
```

`--kernel` and `--firmware` override the `kernel_file` and `firmware.file`
paths from the board RON when you want to use prebuilt blobs without editing
the board file.

## Supported boards

| Board | Platform | Notes |
|---|---|---|
| `qemu-riscv64` | RISC-V 64 | QEMU virt, NS16550, boots Linux via OpenSBI |
| `qemu-aarch64` | AArch64 | QEMU virt, PL011, boots Linux via ATF |
| `qemu-armv7` | ARMv7 | QEMU virt, PL011, boots Linux directly |
| `qemu-riscv64-multi` | RISC-V 64 | Two-stage: bootblock (ROM) → main (RAM) |
| `qemu-aarch64-multi` | AArch64 | Two-stage: bootblock (ROM) → main (RAM) |
| `qemu-riscv64-flex` | RISC-V 64 | Flexible dispatch mode |
| `qemu-aarch64-flex` | AArch64 | Flexible dispatch mode |
| `bananapi-m1` | ARMv7 | Allwinner A20, real hardware, eGON, multi-stage |

The `boards/` directory for each of these is a working example. Copying one
and adjusting the addresses and driver config is the fastest way to add a new
board.
