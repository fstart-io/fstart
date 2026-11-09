# Lenovo ThinkPad X201 bring-up

Arrandale (Ironlake uncore) / QM57 (Ibex Peak) binding ported from
coreboot's `mainboard/lenovo/x201`. Validated on one X201 with an i7 M620
and 8 GiB of RAM, including Linux boot and RTC-timed S3 resume. For what
was ported where, and the remaining differences from coreboot, see
[`docs/ironlake-x201.md`](../../../docs/ironlake-x201.md).

## Build

From the repository root:

```sh
cargo fbuild build --board lenovo-x201 --release --payload uefi-ui \
  --debug ehci --debug flash-console --log-level debug
cargo fbuild assemble --board lenovo-x201 --release --payload uefi-ui \
  --debug ehci --debug flash-console --log-level debug
```

Assembly needs `intel-ucode/06-25-02` and `intel-ucode/06-25-05` from the
pinned `intel-microcode` submodule. In a jj workspace, populate them
separately. Without `--signing-key` the image is signed with an ephemeral
development key.

Debug consoles are opt-in. With `--debug ehci` the platform installs a
best-effort EHCI console; an absent gadget does not prevent flash logging
or boot. `--debug flash-console` reserves a 256 KiB SPI log at flash offsets
`0x520000..0x560000`, after the 128 KiB training cache. The read-only image
then begins at `0x560000`. These offsets describe the board's configured
8 MiB layout; verify the actual machine's descriptor before flashing.
The `uefi-ui` payload provides CrabEFI's setup and boot menu. Use `halt`
instead when deliberately stopping before the installed OS.

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

## Hardware status

On the tested i7 M620 / 8 GiB machine, CrabEFI boots Linux 6.12 from SATA
with the LVDS panel, keyboard, TrackPoint and networking available. All four
logical CPUs are online using their discovered, non-contiguous LAPIC IDs.
PCI ASPM control is granted to Linux.

A 10-second `rtcwake -m mem -s 10` test resumed the same Linux session with
all four CPUs online, MEI enabled and all 24 TPM 1.2 PCRs readable. The kernel
reported one successful suspend and no failures, without MEI or TPM resume
errors. This validates one RTC wake cycle, not every wake source or DIMM
configuration. The GPIO OpRegion overlap warning is still present.

Flash logging and the training cache require firmware SPI writes. This is
a development configuration, not a production flash-lockdown policy; an
ephemeral image-signing key does not establish a production root of trust.

For further hardware validation:

1. Capture a cold boot through `chipset_init`, the QuickPath CPU reset, the
   second raminit pass, the ME UMA handshake, DMI, PCI allocation, SMM, the
   tables and the selected payload. The cold boot reaches the payload after
   exactly one CPU-only reset.
2. Check `memory cache: committed and verified`, then `already current`, and
   suspend/resume from Linux: no retraining, DIMMs held through GPIO10.
3. Validate the native modeset on LVDS, VGA and HDMI, backlight PWM, EC
   fan/thermal, PMH7 input power, radios, docking/undocking (button,
   power-loss and dock-event paths in SMM before ACPI), Fn keys, battery
   thresholds and the X201T digitizer.
4. Check turbo ratios, P-states/C-states and sustained thermal behavior;
   repeat RTC, lid and power-button wake tests, including TPM use after
   repeated resumes.

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
