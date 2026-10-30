# AMD Picasso and Google Vilboz

Picasso (Raven, Raven2; Family 17h model 10h) on the shared
[noncar flow](amd-noncar.md). Board: `boards/google/zork`, one board
`google-vilboz`. Its `Cargo.toml` marks it "build/check/editor support only:
the native boot/image contract is incomplete."

## Code

| Area | Path |
| --- | --- |
| SoC constants, UART and AOAC tables | `crates/driver-amd/src/soc/picasso/mod.rs` |
| Platform: `Config`, `LAYOUT`, `Soc` impl, host policy | `crates/platform-amd/src/picasso/mod.rs` |
| ACPI: interrupt routes, FCH GPIO and I2C AML | `crates/platform-amd/src/picasso/acpi.rs` |
| CCX | `crates/driver-amd/src/ccx/picasso/` |
| DF | `crates/driver-amd/src/df/picasso.rs` |
| NBIO, GPU memory and ATOM info | `crates/driver-amd/src/nbio/picasso/` (`gfx.rs`, `late.rs`, `tables.rs`) |
| DXIO | `crates/driver-amd/src/dxio/picasso/` |
| SMU v10, power policy | `crates/driver-amd/src/smu/v10/` |
| FCH (Sandstone) | `crates/driver-amd/src/fch/sandstone/` (`preinit`, `legacy`, `usb`, `pinmux`); `fch/emmc.rs`, `fch/espi.rs`, `fch/i2c.rs`, `fch/gpio.rs` |
| Option ROM and VBE | `crates/driver-vbios` |
| Board | `boards/google/zork/src/`: `lib.rs` (`Config`, hooks), `policy.rs` (SPI, eSPI, PCIe, USB, eMMC, displays, CBI decode), `dalboz.rs` (GPIO and PCIe tables), `vilboz.rs` (RAM-ID and WiFi GPIOs), `ec.rs` (CBI from the EC), `smm.rs` (FCH SMI hooks) |
| AMD firmware packing | `crates/image-build/src/amd/` (`assembly.rs`, `apcb.rs`, `blobs.rs`, `psp_app.rs`) |
| Firmware file tables | `crates/platform-amd/Cargo.toml` (`[package.metadata.fstart.amd.picasso]`), `boards/google/zork/Cargo.toml` (`[package.metadata.fstart.amd]`) |

## SoC constants

| Constant | Value |
| --- | --- |
| `MAX_DIES` | 1 |
| ECAM | `0xf800_0000`, 64 buses |
| Console UARTs | `0xfedc9000`, `0xfedca000`, `0xfedce000`, `0xfedcf000` (FCH UART 0-3) |
| `MAX_CPUS` | 16 |
| `TSEG_SIZE` | 8 MiB |
| PSP variant | `psp::Variant::Rv` |
| APOB | `0x0200_1000`, `0x1_0000` bytes |
| BIOS directory | `Family17hModel10` |

## Board configuration (Vilboz)

`boards/google/zork/src/lib.rs`:

| Item | Value |
| --- | --- |
| Console | FCH UART 0, 115200 baud, 48 MHz clock |
| SPI, eSPI | `policy::SPI`; eSPI to the EC with `dalboz::ESPI_GPIO_TABLE` |
| Early GPIO tables | `EARLY`, `TPM`, `PCIE`, `ROMSTAGE` (in that order) |
| ROM sharing | Disabled (`DISABLE_SPI_FLASH_ROM_SHARING`) |
| I2C2 (400 kHz) | ELAN0000 at `0x15`; SYNA0000 or PNP0C50 at `0x2c` (HID descriptor `0x20`). Both level-triggered, active-low, on GPIO 9. One is fitted. |
| I2C3 (400 kHz) | GOOG0005 (Cr50 TPM) at `0x50`; ready interrupt on GPIO 3, falling edge |
| Graphics | `policy::DISPLAYS`, `hdmi2_disable: 2` |
| eMMC | Present only if the CBI says so |
| Power | `None` in `Config`; set from the CBI in `fch_reset_done` |

Board hooks:

- `fch_reset_done`: reads the CBI from the EC (`ec::read_cbi`). Sets
  `config.power` (`Cbi::native_power()`) and `config.emmc`. Then it configures
  the RAM-stage GPIOs (`vilboz::configure_ram`).
- `acpi()`: the keyboard behind the EC, from `driver-chromeec`.
- `smm.rs`: `VilbozSmm`, the board part of the FCH SMI handler, built only with
  `fstart_stage_env = "smm"`.

RAM-ID straps are GPIO 132, 90, 86 and 69 (`lib.rs`). They are sampled before
GPIO 86 is driven high. The APCB instance is chosen per RAM-ID value. FW_CONFIG
fields select WWAN, NVMe, eMMC, SAR and audio options (`policy.rs`).

## Silicon init

`picasso::Picasso::silicon_init` (TP1), in order:

1. `fch_pre_init`: legacy decode, AOAC power of the early devices and of the
   console UART, console pads. Runs before the console.
2. `identify`: CPUID signature and extended features. The silicon ID comes from
   NBIO early init.
3. DF legacy routes and early init, NBIO early init, PSP BAR, BSP CCX init
   (MCA), IOHC DRAM routes.
4. `fch_init`: LPC prepare, eSPI link (if configured), SPI, SMBus, the board's
   early GPIO tables, ROM-sharing disable.
5. Board `fch_reset_done`: CBI, power policy, eMMC.
6. IOAPIC decode, IOMMU BAR.
7. DXIO training (`dxio::picasso::train`, balanced power policy), then board
   `dxio_training_done`.
8. NBIO late pre-enumeration. The SMU power policy comes from `config.power`,
   which is required.
9. Sandstone ALink, ACPI decode, legacy LPC, clocks, USB PHY and controllers,
   DF late init, eMMC (if configured).
10. MP (`noncar::mp::start`) with the per-thread cache policy captured from the
    PSP MMIO base.
11. SMU CCX-done. If the SoC reports a GPU: `nbio::gfx::initialize` uses the
    reserved UMA and prepares the GPU memory map and ATOM system information.
    It does not set a display mode.
12. Post-silicon NBIO init, FCH ACPI ports, SMM (`noncar::smm::install`), PSP
    SmmInfo (`noncar::psp::notify_smm`).
13. eMMC ACPI device `MMC0` (`AMDI0040`) and the I2C controllers with their
    devices in `\_SB`.

TP2 (`after_resources`) sets the FCH ACPI ports. Then `Board::mainboard_final`.

TP3 (`ready_to_boot`), after enumeration:

- NBIO late after-enumeration.
- If there is a GPU: find the VGA device (class 3, vendor `0x1002`). Its ROM is
  `pciVVVV,dddd.rom`. Raven2 keeps PCI ID `1002:15d8` but uses
  `pci1002,15dd.rom`, selected by the silicon identity.
- Read the ROM with `ffs_context::read_verified_asset`. Only an authenticated
  ROM is run.
- `fstart_driver_vbios::initialize` runs the native Rust rsemu interpreter,
  with bounded RAM and a deadline, and selects a VBE linear-framebuffer mode. The
  result is `FramebufferInfo`, which goes to the payload.
- `Board::ready_to_boot`.

## Memory layout

From `picasso/mod.rs`:

| Use | DRAM range |
| --- | --- |
| Early region (reserved through the payload) | `02000000..03400000` |
| APOB (PSP writes it) | `02001000..02011000` |
| Reset page tables | `02020000..02026000` |
| Data and BSS | `02030000..02200000` |
| Heap | `02200000..02a00000` |
| BSP stack | `02f00000..03000000` |
| Reset image (PSP copies it here) | `03000000..03400000` |
| Reset vector | `033ffff0` |

The image reservation is 4 MiB, including the final 8 KiB for the 16-bit
entry. The former 2 MiB reservation was a framework limit, not a hardware
limit; the larger reservation accommodates the Rust interpreter and graphical
UEFI. The generated linker script, BIOS directory and e820 reservation all
use this layout.

TSEG: 8 MiB, below the low top of DRAM. The AP trampoline page at `0x8000` is
reserved in e820.

## Flash image

`boards/google/zork` is packed from blobs (`Firmware::Packed`) with a reset
offset, so the result is a whole flash image.

| Flash offset | Use |
| --- | --- |
| `000000..780000` | Erased (`0xff`) |
| `780000..b80000` | 4 MiB reset-image window; stored zlib-compressed (`compress_reset`), BIOS entry 0x62 |
| `b80000..e1f000` | FFS: payload, `cpu_microcode.bin`, the VBIOS assets |
| `e20000..1000000` | PSP partition: EFS, PSP and BIOS directories, firmware blobs, APCBs, fstart's PSP application |

The reset offset is derived from `picasso/mod.rs`'s layout: the full reset
image window ends at the fixed FFS start. Growing it moves its start earlier
in erased flash without shrinking the FFS or moving the PSP partition. The
flash chip remains 16 MiB.

The PSP partition geometry and SPI settings come from
`crates/platform-amd/Cargo.toml`. The EFS `efs-offset` is `0xe20000`,
`partition-end` is `0x1000000`, and the SPI is mode 4, speed 0.

### Blobs

`crates/platform-amd/Cargo.toml` lists the Picasso files by path, relative to
the blob directory: PSP entries, BIOS entries (DDR4 training images, types
0x64/0x65), microcode for equivalence IDs `0x8180`, `0x8181` and `0x8201`, and
the VBIOS files for device `0x15d8` and `0x15dd`. The PSP entries follow
amd_blobs' Picasso `fw.cfg` with the secure OS and without MP2 (soft-fuse bits
28 and 29 set).

`boards/google/zork/Cargo.toml`:

```toml
[package.metadata.fstart.amd]
soc = "picasso"
psp-app = true          # pack fstart's PSP application
apcb = { instances = 13 }
```

- `apcb = { instances = 13 }`: the APCBs are `google-vilboz/apcb/<n>.bin` for
  `n` in `0..13`. They are extracted from a vendor image; they are not generated.
- `psp-app = true`: fstart's PSP application (`image-build/src/amd/psp_app.rs`)
  is packed as PSP entry 0x52. The PSP of this part starts the x86 cores only
  after an application ran there.

Extract the blobs once, or pass the vendor image as `--donor` to build them on
first use:

```sh
cargo fbuild blobs --board google-vilboz --from vilboz-backup.rom --out blobs/
```

## Build

```sh
cargo fbuild assemble --board google-vilboz --release --payload uefi-ui --blobs blobs/
cargo fbuild assemble --board google-vilboz --release --payload uefi-ui --donor vilboz-backup.rom
```

The output is `google-vilboz.rom` next to its `.json`. Picasso's stage and SMM
units are release-only. Payloads: `halt` (default), `linux`, `uefi`, `uefi-ui`,
`uefi-basic`.

Payload differences:

- `uefi`, `uefi-ui` and `uefi-basic` hand the board's I2C devices to CrabEFI as
  peripherals (`Peripherals`, feature `payload-uefi-basic`). Which ones CrabEFI drives
  depends on its features. The touchpad drivers (`crabefi-elan-i2c`,
  `crabefi-hid-i2c`) come only with `payload-uefi-ui`.
- The same I2C devices are in ACPI (`\_SB`, from `picasso/acpi.rs`), with their
  GPIO interrupts.

## Not implemented

- Suspend: the DSDT (shared, `crate::acpi`) declares S0, S1, S4 and S5 only. There is no S3 resume path.
