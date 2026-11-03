# Lenovo ThinkPad X201 bring-up

Experimental Arrandale (Ironlake uncore) / QM57 (Ibex Peak) binding ported
from coreboot's `mainboard/lenovo/x201`. **Not hardware-validated.** For what
was ported where, and the remaining differences from coreboot, see
[`docs/ironlake-x201.md`](../../../docs/ironlake-x201.md).

## Build

From the repository root:

```sh
cargo fbuild build --board lenovo-x201 --release --payload halt
cargo fbuild assemble --board lenovo-x201 --release --payload halt
```

Assembly needs `intel-ucode/06-25-02` and `intel-ucode/06-25-05` from the
pinned `intel-microcode` submodule. In a jj workspace, populate them
separately. Without `--signing-key` the image is signed with an ephemeral
development key.

The binding includes the bootblock (CAR, DDR3 raminit with the QuickPath
CPU reset), postcar, ramstage and SMM. Raminit replays the cached training
record on S3 resume; the record lives in the 128 KiB `mrc-cache` at the
start of the BIOS region and the ramstage writes it over the PCH SPI
controller after a cold boot. `_S3_` is advertised only once that record
persisted.

## Board facts

| Item | Value | coreboot source |
| --- | --- | --- |
| SPD addresses | `0x50` (ch0), `0x51` (ch1) | `romstage.c` |
| Clock generator | SMBus `0x69`, FSB word 62879 | `romstage.c` |
| Console | EHCI debug port, EHCI2 `00:1a.0`, BAR `0xfef00000` | `USBDEBUG_HCD_INDEX=2` |
| DRAM reset gate | GPIO10, driven low on S3 entry | `DRAM_RESET_GATE_GPIO` |
| EC | H8 at `0x62/0x66`, `0x1600/0x1604`; PMH7 at `0x15e0` | `devicetree.cb` |
| EC SCI / wake | GPI1 (GPE `0x11`) / GPI13 | `smihandler.c` |
| Dock | ID straps GPIO3..5, connect GPIO28 | `dock.c` |
| Bluetooth card | GPIO48, low when fitted | `bdc_gpio_num` |
| Digitizer | PC87382 LDN 3 at `0x200`, IRQ 5 | `devicetree.cb` |
| Display | LVDS panel; HDMI2, VGA, dock DP | `gma-mainboard.ads` |
| VBT | `data.vbt` | `INTEL_GMA_HAVE_VBT` |

The X201 has no serial port. The EC switches the USB power on before the
debug console starts, as coreboot's `bootblock_mainboard_early_init()` does.
Connect a high-speed USB debug gadget directly to the root port wired to
EHCI2's debug port.

## Flash safety

This configuration describes coreboot's 8 MiB layout:

| Region | Offset | Size |
| --- | ---: | ---: |
| Descriptor | `0x000000` | `0x001000` |
| GbE | `0x001000` | `0x002000` |
| ME | `0x003000` | `0x4ed000` |
| BIOS | `0x500000` | `0x300000` |

Vendor descriptor, ME and GbE data are **not bundled**. Read and verify a
complete backup from the actual machine and keep an external programmer
around. As coreboot notes (`ME_CLEANER_ARGS`), a cleaned ME needs its EFFS
partition (`me_cleaner -S -w EFFS`) or USB and SATA misbehave.

## First hardware checks

1. Capture a cold boot through `chipset_init`, the QuickPath CPU reset, the
   second raminit pass, the ME UMA handshake, DMI, PCI allocation, SMM, the
   tables and the halt payload. The cold boot reaches the payload after
   exactly one CPU-only reset.
2. Check `memory cache: committed and verified`, then `already current`, and
   suspend/resume from Linux: no retraining, DIMMs held through GPIO10.
3. Validate the native modeset on LVDS, VGA and HDMI, backlight PWM, EC
   fan/thermal, PMH7 input power, radios, docking/undocking (button,
   power-loss and dock-event paths in SMM before ACPI), Fn keys, battery
   thresholds and the X201T digitizer.
4. Check both cores and their threads come up, the turbo ratios, the
   P-states/C-states, and that the TPM (if fitted) appears as `PNP0C31`.

Host validation:

```sh
cargo test -p fstart-arch --features x86_64
cargo test -p fstart-driver-intel --features ironlake,acpi,display --lib
cargo test -p fstart-platform-intel --features ironlake-ibexpeak,acpi
cargo test -p fstart-driver-lenovo --features fstart-arch/x86_64,acpi
```

The board crate's own tests (GPIO, HDA and flash facts against coreboot, the
clock generator and an `iasl` DSDT round trip) run from the workspace fbuild
generates: `cd target/fstart-workspaces/lenovo-x201 && cargo test -p
fstart-board-lenovo-x201 --lib`.
