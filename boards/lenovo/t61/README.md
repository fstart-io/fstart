# Lenovo ThinkPad T61 / T61p

Experimental GM965/ICH8M-E board support. **Not hardware validated.**
Socket P selects the existing Merom or Penryn CPU driver at runtime. PEG display
probing selects a discrete GPU when present; otherwise the integrated GMA is
used. The laptop Ultrabay, Mini Dock serial port, H8/PMH7, audio, SATA ports 0/2,
PCIe ports 1–5, identity EEPROM and Ricoh SD policy are wired in board code.

```sh
cargo fbuild assemble --board lenovo-t61 --release --payload halt
cargo fbuild test-library --board lenovo-t61
```

## NVIDIA display

The Quadro NVS 140M (`10de:0429`) and FX 570M (`10de:040c`) need their matching
legacy video ROM. Obtain it from your own original firmware backup; it is not
included here. A firmware extractor typically names them `oprom_1.rom` and
`oprom_0.rom`, respectively. Keep private binaries out of version control.

Pass an explicit Intel file-asset configuration, for example:

```json
{
  "file_assets": [
    { "name": "pci10de,0429.rom", "path": "private/t61-nvs140m.rom" }
  ]
}
```

```sh
cargo fbuild assemble --board lenovo-t61 --release --payload halt \
  --option file-assets=/path/to/t61-assets.json
```

Asset paths are relative to the repository root (absolute paths also work).
The image assembler signs these assets. The shared bounded VBIOS executor
checks PCI identity and checksum, POSTs the selected GPU, exports its VBE
framebuffer and publishes the posted ROM through ACPI `_ROM`. Without the
matching asset, boot continues headlessly. Use the `040c` name and matching ROM
for an FX 570M. The G8x panel ACPI methods use a CCFL-safe minimum duty cycle;
brightness keys notify the active GPU's panel. Integrated graphics use the
board's 220 Hz PWM policy and VBT instead. VBIOS execution and modesetting are
skipped on S3 resume; the OS restores the display.

## CPU voltage option

`cpu_undervolt` is exposed through fstart's persistent options and payload
settings menu on both T61 and X61. Its value is a number of **12.5 mV steps**:
0 is stock voltage, 8 is a 100 mV reduction, and 12 is 150 mV. It applies to
ACPI P-states and Penryn EMTTM, never reducing a normal state below the CPU's
lowest normal VID. SLFM states already below that floor are unchanged.

**Undervolting can cause instability or data corruption.** It is opt-in and
not a guaranteed operating point. Restore the option to zero to disable it.

## Flash precautions

The factory 4 MiB layout differs from the X61:

```
00000000:00000fff descriptor
00001000:0027dfff ME
0027e000:0027ffff GbE
00280000:003fffff BIOS
```

Preserve the original descriptor, ME and GbE regions, and keep verified backups.
Only write the BIOS region (`0x280000..0x400000`) of an assembled full image.
The configured SPI VSCC is `0x2005` for a W25X32: 4 KiB erase opcode `0x20`.
**Check the fitted flash first.** SST parts require `0x2009` instead; change the
board's `.spi_vscc(...)` value before building for them.

Use hardware sequencing (`flashrom -p internal:ich_spi_mode=hwseq`) and an
explicit region layout for internal updates. Software sequencing can choose a
whole-chip erase and destroy regions that cannot be restored internally.
The original firmware can protect the final 64 KiB. This board does **not**
currently generate a redundant Top Swap bootblock: do not attempt a Top Swap
installation with this image. Use an external programmer for initial testing.

## Validation and limitations

Host tests cover the T61 interrupt/GPIO/audio policy, complete ACPI
assembly/disassembly, all panel brightness levels and live PWM readback. Driver tests cover
RCOMP population, cached-training I/O state, DIMM sensor gating, PEG policy and
VID clamping. These are not evidence of a successful hardware boot.

Cold boot, both GPU variants, accelerated graphics, thermal behavior, S3,
internal flashing and dock hotplug still need hardware validation. TPM,
digitizer, dock parallel/IR and T60-style legacy Ultrabay I/O modules are not
supported. The dock and EC resource windows remain reserved even when undocked.
