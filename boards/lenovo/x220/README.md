# Lenovo ThinkPad X220 bring-up

Sandy Bridge / QM67 (Cougar Point) binding ported from coreboot's
`mainboard/lenovo/x220` (variant `x220`). **Not tested on hardware.** For
what was ported where, and the remaining differences from coreboot, see
[`docs/sandybridge-x220.md`](../../../docs/sandybridge-x220.md).

## Build

From the repository root:

```sh
cargo fbuild assemble --board lenovo-x220 --release --payload uefi \
  --debug ehci --log-level debug
```

Assembly needs `intel-ucode/06-2a-07` and `intel-ucode/06-3a-09` from the
pinned `intel-microcode` submodule. Without `--signing-key` the image is
signed with an ephemeral development key.

Debug consoles are opt-in. `--debug ehci` installs a best-effort console on
EHCI2's debug port (00:1a.0, coreboot `USBDEBUG_HCD_INDEX=2`); connect the
gadget directly to the root port wired to it. `--debug flash-console`
reserves a 256 KiB SPI log after the 128 KiB training cache.

## Board facts

| Item | Value | coreboot source |
| --- | --- | --- |
| SPD addresses | `0x50` (ch0), `0x51` (ch1); DDR3-1333 cap | `devicetree.cb` |
| Console | EHCI debug port, EHCI2 `00:1a.0`, BAR `0xfef00000` | `USBDEBUG_HCD_INDEX=2` |
| DRAM reset gate | GPIO10, driven low on S3 entry | `DRAM_RESET_GATE_GPIO` |
| EC | H8 at `0x62/0x66`, `0x1600/0x1604`; PMH7 at `0x15e0` | `devicetree.cb` |
| EC SCI / wake | GPI1 (GPE `0x11`) / GPI13 | `smihandler.c` |
| WWAN presence | GPIO70, low when fitted | `wwan_gpio_num` |
| PCIe | RP2 WLAN, RP4 ExpressCard (hot-plug), RP5 RCE822, RP7 xHCI option; coalesced | `devicetree.cb` |
| Display | DP1..3, HDMI1..3, VGA, LVDS panel | `gma-mainboard.ads` |
| VBT | `data.vbt` | `INTEL_GMA_HAVE_VBT` |

## Flash safety

This configuration describes the stock 8 MiB layout:

| Region | Offset | Size |
| --- | ---: | ---: |
| Descriptor | `0x000000` | `0x001000` |
| GbE | `0x001000` | `0x002000` |
| ME | `0x003000` | `0x4fd000` |
| BIOS | `0x500000` | `0x300000` |

Vendor descriptor, ME and GbE data are **not bundled**. Read and verify a
complete backup from the actual machine, flash only the BIOS region and
keep an external programmer around.

Host validation of the board crate (GPIO, HDA and flash facts against
coreboot, and an `iasl` DSDT round trip):

```sh
cargo test -p fstart-board-lenovo-x220 --lib
```
