# Lenovo ThinkCentre M700 / M900 Tiny

Experimental Kaby Lake (or Skylake) desktop with a Sunrise Point-H PCH and an
NCT6687D Super I/O, on fstart's native Skylake/Kaby Lake support: DDR4
training, the system agent and the PCH run without FSP. It has booted Linux
through CrabEFI; cold and cached memory training are not yet qualified across
DIMM populations.

```sh
cargo fbuild assemble --board lenovo-m900-tiny --release --payload uefi
```

The image needs the CPU microcode (`intel-microcode/`, for the FIT) and the
board's `data.vbt`. The output is the whole 16 MiB flash; flash only its BIOS
region over the machine's own descriptor, ME and GbE regions, or let
`fbuild compose-bundle --intel-flash BACKUP` merge it into a backup.

The console is COM1 (0x3f8, 115200 8N1) on the optional serial header; port
0x80 POST codes cover the time before it. Warm reset and S3 are not
supported: the PCH and memory init only do a cold boot.

## Boot display

Ramstage publishes the verified `data.vbt` through the IGD OpRegion. The Rust
Gen9 backend probes the three DP/HDMI connector pairs from coreboot's board
port list. It can mirror one XRGB framebuffer on up to three displays, with
separate timing, PLL, and scaler state per pipe. The fastest selected mode
sets the shared display clock, subject to the chip's fuse limit. Confirmed
LSPCON adapters switch to protocol-converter mode before EDID probing. The
payload gets a framebuffer only if an output comes up. No Ada library,
FSP GOP, or VBIOS runs for this path.

The backend also supports a board-selected eDP panel with VBT power timing
and PWM backlight data. M900 does not select eDP because it has no internal
panel. There is no native LVDS or analog VGA DAC on this generation. An
optional VGA module would need to work through its digital converter; that
module, DP MST, and physical video output have not been qualified.

## Source

The silicon code is ported from a native (FSP-less) Kaby Lake coreboot tree,
`src/soc/intel/skylake/romstage/native/`, and the board from
`src/mainboard/lenovo/m900_tiny/`.
