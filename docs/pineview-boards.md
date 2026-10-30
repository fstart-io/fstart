# Pineview boards

Reference for the Intel Atom D4xx/D5xx (Pineview, NM10/ICH7) boards.

- Platform: `PineviewIch7<fstart_platform_intel::legacy_cpu::Fcbga559>`, in
  `crates/platform-intel/src/pineview.rs` (feature `pineview-ich7`).
- Northbridge: `crates/driver-intel/src/pineview/` (`mod.rs`, `regs.rs`,
  `raminit/` for DDR2 training).
- Southbridge: `crates/driver-intel/src/ich7/`.
- Board crates: `boards/intel/d510mo`, `boards/gigabyte/ga-d510ud`,
  `boards/foxconn/d41s`. These are the only crates with `pineview-ich7` in
  `boards/*/*/Cargo.toml`.

## Boards

| Board | `-b` name | Super I/O (LPC PnP port) | SPI flash (`FLASH_SIZE`) |
| --- | --- | --- | --- |
| Foxconn D41S | `foxconn-d41s` | IT8721F (`0x2e`) | 16 MiB |
| Intel D510MO | `intel-d510mo` | W83627THG (`0x4e`) | 1 MiB |
| Gigabyte GA-D510UD | `gigabyte-ga-d510ud` | IT8720F (`0x2e`) | 512 KiB |

The `-b` name is `[package.metadata.fstart] board` in each `Cargo.toml`.
Flash is a contiguous legacy mapping (`FlashLayout::X86Legacy`). No IFD.

## Build

```sh
cargo fbuild build    -b intel-d510mo --release --payload halt
cargo fbuild assemble -b intel-d510mo --release --payload halt
```

- `build --stage <name>` builds one compilation unit and its producers.
- `--payload` values: `uefi`, `uefi-ui`, `uefi-basic`, `linux`, `coreboot`,
  `halt`.
- `assemble` also takes `--kernel`, `--firmware` and `--fit`.
- SeaBIOS (`--payload coreboot`): see
  [`ci/payloads/seabios/README.md`](../ci/payloads/seabios/README.md).

## Board hooks

| Board | `car` / `ram` Super I/O setup | CK505 clock gen (SMBus `0x69`) |
| --- | --- | --- |
| D41S | `before_console`: IT8721F init, disconnect HW-monitor IRQ, disable parallel ECP decode, disable floppy LDN | `after_memory` (car, skipped on S3 resume) and `after_devices` (ram). Failure is logged; boot continues. |
| D510MO | `before_console`: W83627THG init, `enable_hwmon(0x290)` | `after_devices` (ram, skipped on resume) |
| GA-D510UD | `before_console`: IT8720F init, GPIO table at `0x800`, internal power-good, watchdog off, BSEL preset, main flash bank, HW-monitor IRQ disconnected, parallel ECP decode disabled | none |

Source: `boards/*/*/src/mainboard.rs`.

## Board configuration

All three use `PineviewIch7Config::new()` with `max_cpus(4)`, IGD on the VGA
port, and the VBT from FFS asset `data.vbt`.

| Setting | D41S | D510MO | GA-D510UD |
| --- | --- | --- | --- |
| PCIe root ports enabled | 0, 1 | 0, 1, 2, 3 | 0, 1 |
| PIRQ routing | `ICH7_PINEVIEW_ROUTING`, `[11; 8]` | `ICH7_PINEVIEW_ROUTING` with bridge routes overridden (`A→G, B→E, C→B, D→A`), `[11; 8]` | `ICH7_PINEVIEW_ROUTING`, `[11; 8]` |
| Integrated LAN | on (default) | off (`.lan(false)`) | on (default) |
| Early serial IRQ | on (default) | off: Quiet until `ramstage_init` | on (default) |
| LPC fixed I/O | COM1, COM2, LPT `0x378` | same | same |
| LPC generic I/O | `0x0a00`, size `0x100` | `0x290`, size 8 | `0x290`, size 8 |
| SATA | AHCI, ports 0, 1 | same | same |
| USB | EHCI, UHCI ×4 | same | same |
| GPE0 events | Thermal, TcoSci, BatteryLow | TcoSci, GPIO 13 | Thermal, TcoSci, BatteryLow |
| HDA codec (vendor:subsystem) | `10ec:0662` / `105b:0d55` | `10ec:0662` / `8086:d618` | `10ec:0887` / `1458:a002` |
| Super I/O ACPI name | `SIO0` | `SIO0` | `SIO0` |

Shared fixed values (`crates/platform-intel/src/pineview.rs`):

| Window | Address |
| --- | --- |
| MCHBAR | `0xFED1_0000` |
| DMIBAR | `0xFED1_8000` |
| EPBAR | `0xFED1_9000` |
| ECAM (PCIEXBAR) | `0xE000_0000`, 256 buses |
| RCBA | `0xFED1_C000` |
| PMBASE | `0x0500` |
| SMBus base | `0x0400` |

Northbridge defaults (`crates/driver-intel/src/pineview/mod.rs`):

- DIMM SPD addresses: `0x50`, `0x51` (slots A/B).
- Integrated graphics: stolen memory defaults to 16 MiB. Allowed values are
  8, 16, 32, 48, 64, 128, 256 MiB.
- IGD device IDs: `0xa001` (desktop), `0xa011` (mobile).

Board configs: `boards/*/*/src/config.rs`.

## Super I/O drivers

| Chip | Driver | Notes |
| --- | --- | --- |
| IT8721F | `crates/driver-superio/src/ite8721f.rs` | Uses the shared `SuperIoConfig` directly. Adds `disconnect_hwmon_irq`, `disable_parallel_ecp_decode`, `disable_floppy`. |
| IT8720F | `crates/driver-superio/src/ite8720f.rs` | GPIO via `ite_gpio.rs`; BSEL preset; watchdog; DualBIOS bank select. |
| W83627THG | `crates/driver-superio/src/w83627thg.rs` | Chip-ID mask ignores the revision nibble. KBC and mouse share LDN 5 with separate IRQ slots. HW monitor uses an indexed pair at HWMBASE+5/+6. |

D41S has no IT8721F GPIO configuration (`gpio: None`).

### IT8720F (GA-D510UD)

- GPIO is configured per physical pin. `GpioPinConfig::input(40)` is GP40,
  not bit 0 of a group. Updates are field-level read-modify-write, so
  unlisted pins are preserved. The driver does not write GPIO activation
  (CR30).
- GP45 is rejected (reserved). Pull-ups are allowed only on pins that have
  them.
- `simple_io(22)` keeps the strap direction. `input(40)` forces input.
- BSEL latches are preloaded to `BSEL0=1, BSEL1=0, BSEL2=0`. Outputs are not
  enabled.
- DualBIOS: LDN 7 `EF` bit 0 (`BANK`) is set to Main (0) by read-modify-write.
  Other bits are preserved.
- LDN 7 `FC` (VID input) is not written by the driver.

## Clock generator

`crates/driver-intel/src/generic/ck505.rs`: IDT CK505 over SMBus. Programs the
full register block (mask + regs) with one block write. Used by D41S and
D510MO only.

## PIRQ and GPE encoding

- `crates/driver-intel/src/southbridge/pirq.rs` owns the PIRQ map. It produces
  the RCBA `DxxIR` values and the ACPI `_PRT`.
- `Ich7Gpe::Gpio(n)` takes a physical GPIO number (0–15). It encodes to the
  GPE0 bit for that pin inside the driver.
- `SataConfig::new(SataMode::Ahci, [0, 1])` takes port numbers, not a bitmap.

## Limitations

- No Realtek RTL8168 firmware/MAC-load code and no JMicron JMB363 init exist
  in the tree. GA-D510UD's PCIe ports 0 and 1 are enabled, but generic PCI
  enumeration does not replace device-specific init.
- The D41S 16 MiB layout does not infer erase geometry or variable-store
  capacity.

## Related

- [`docs/smm.md`](smm.md): Pineview/Atom SMM layout.
- [`ci/payloads/seabios/README.md`](../ci/payloads/seabios/README.md): SeaBIOS
  payload recipe.
