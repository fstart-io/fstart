# Lenovo ThinkPad X200 bring-up

Experimental GM45/ICH9-M binding, following coreboot's `lenovo/x200`.

## Hardware status

One X200 (7459WST: P8400, 2 x 4 GiB DDR3, W25X64 with an ME-less descriptor)
cold-boots Linux through CrabEFI. SATA, e1000e, the AR9280 WLAN, i915 on the
panel, ACPI backlight, EC/thinkpad_acpi, thermal zones, HDA, acpi-cpufreq,
C1-C3 and DMA remapping work. Warm resets, S3 resume, the dock, the X200t
digitizer and other DIMM/CPU combinations are untested.

## Build

From the repository root:

```sh
cargo fbuild build --board lenovo-x200 --release --payload halt
cargo fbuild assemble --board lenovo-x200 --release --payload halt
```

Assembly needs the pinned `intel-microcode` submodule populated, including
`intel-ucode/06-17-06` and `intel-ucode/06-17-0a`. In a jj workspace these files
may need to be populated separately. The assembler generates ignored local
development keys; this is a development-integrity image without rollback
protection, not production signing policy.

The halt payload is for bring-up, not an OS boot test. The binding includes
bootblock, postcar, ramstage and SMM. Release assembly has been checked;
unoptimized/debug postcar currently exceeds its 64 KiB image budget. DDR3 single-channel or paired DIMMs (interleaved
like coreboot) are supported; DDR2 is not.

S3 resume follows coreboot's `raminit(s3resume)`. It skips the DDR3 reset
and JEDEC init, and replays DDR3-1067 read/write training instead of
retraining. The record (SPD bytes, clock, CAS and trained fields) lives in
the 128 KiB `mrc-cache` at the start of the BIOS region. The mainstage
writes it over ICH9 SPI after a cold boot whose probes pass, and only
when it changed. `_S3_` is advertised only once that record and the stage
caches persisted. Flash writes need a W25Q/W25X/MX25L part or the
SST25VF064C; the SST's block protection is cleared through EWSR + WRSR
around each write and then restored. Resume restores ASLS to the OpRegion kept in the firmware store
and re-runs panel power and backlight setup; the OS restores the display
mode.

## EC and OS policy

X200 advertises 100°C critical / 90°C passive thermal trips, with an initial
80°C active-cooling trip. The shared H8 emitter handles Fn-F2/Fn-F3 and both
batteries' `_BIF`/`_BIX`, including cycle count. Its X200 threshold backend
uses selector-based `0x24/0x25`; the `0xb0` backend remains for later ThinkPads.

SMBIOS supplies the non-OEM `CBET4000 fstart …` ID, ThinkPad product version,
and the real EC firmware OEM string when readable, for Linux `thinkpad_acpi`
identification. An EC read failure is logged and the OEM string is omitted.
The shared SMM handler handles a short power-button press before OS ACPI
ownership, while leaving SCI-owned PM1 events to the OS.

These interfaces have not been qualified on physical hardware.

## EHCI console

X200 has no onboard serial console: it logs to the EHCI debug port, through
the shared `fstart-driver-usbdebug` driver:

* EHCI2, PCI **00:1a.7**, following coreboot's `USBDEBUG_HCD_INDEX=2`.
* The hardware debug port advertised by HCSPARAMS (ICH9 port 1).
* Fixed BAR0 **0xfef00000** (coreboot i82801ix `CONFIG_EHCI_BAR`), with a
  4 KiB reservation retained by the PCI allocator. No schedule/DMA allocation is needed.
* Connect a **high-speed USB debug gadget** with a USB debug descriptor.
  Examples are an appropriately programmed FX2 gadget or a compatible USB
  debug cable. An ordinary FTDI/CP210x/CH340 USB-to-UART cable does not work.
  Hubs are not supported; connect the gadget directly.
* Run the gadget's matching capture software on a second host before powering
  on the X200. The console does not impose a UART baud rate.

Connect the gadget to the socket wired to root port 1 under PCI 00:1a.7;
`lsusb -t` and `/sys/bus/usb/devices` in a running OS map ports to sockets.

The bootblock resets the controller and the debug port and enumerates the
gadget at address 127; a short `USB` greeting marks that enumeration.
Descriptor discovery also tries reserved address 127 if address 0 fails,
without reassigning an already-addressed gadget. Postcar and the ramstage
find the debug port still claimed and running and only re-attach: they read
the endpoints back from the gadget and reset both bulk data toggles with
`CLEAR_FEATURE(ENDPOINT_HALT)`, as coreboot carries the pipe state through
CAR and CBMEM instead of resetting the port. If re-attaching fails, the stage
falls back to a full reset and enumeration.

Output is packed into 8-byte packets and flushed at the end of every log
line, like coreboot's `usb_tx_byte`/`usb_tx_flush`. A packet the host does
not take within coreboot's 1000 NAK retries is dropped; until the host drains
again, later packets only get a short retry budget so an absent capture does
not slow the boot down. Bus errors are retried, a hardware error or STALL
takes the console offline. Missing or incompatible gadgets leave the console
offline without stopping boot; `EhciDebug::online()` can distinguish that
condition. There is no alternate UART fallback, so a silent capture is not by
itself evidence of a boot hang.

## Flash safety

This configuration describes the **6D, 8 MiB** layout, not the 7X/4 MiB layout:

| Region | Offset | Size |
| --- | ---: | ---: |
| Descriptor | `0x000000` | `0x001000` |
| ME | `0x001000` | `0x5f5000` |
| GbE | `0x5f6000` | `0x002000` |
| PDR | `0x5f8000` | `0x008000` |
| BIOS | `0x600000` | `0x200000` |

Vendor descriptor/ME/GbE/PDR data are **not bundled**. Read and verify a complete
backup from the actual machine, inspect its descriptor, and retain an external
programmer/recovery path. A successful fbuild assembly does not make a generic
full-flash image safe to write: preserve this machine's non-BIOS regions.
The firmware does not check the descriptor at run time: it only needs the top
2 MiB of the BIOS region, so an ME-less descriptor whose BIOS region starts
lower also boots. Write just the BIOS region (`--ifd`, region `bios`).
From Linux, use rflasher's internal programmer: flashrom 1.8 erases this
board's W25X64 with 32 KiB block erases the part does not support.

## First hardware checks

1. Verify the descriptor, CPU/DIMM topology, panel and debug-port wiring
   against the actual machine. As in coreboot, the LVDS EDID string selects
   the backlight PWM frequency, and a known frequency of 200 Hz or less
   selects `data_ccfl.vbt` instead of `data_led.vbt`.
2. Capture a cold boot through RAM training, probes, DMI/VC1, PCI allocation,
   SMM installation, tables and halt. Confirm the EHCI BAR stays fixed.
3. Repeat without a gadget and after an interrupted/warm boot. Confirm there
   is no reset loop or unintended power-off.
4. Check the log reports `memory cache: committed and verified`, then
   `already current` on later cold boots at DDR3-800 (DDR3-1067 retrains, so
   its record can change). Suspend from Linux and resume: no retraining, no
   memory probe, ASLS restored, backlight and display back, and no memory
   corruption across repeated cycles. A resume without a matching record
   (swapped DIMM, cleared cache) must take a prepared cold reset.
5. Exercise each admitted RAM topology and run extensive memory testing
   independently; the firmware's sparse probes are not RAM qualification.
6. Validate EC fan/thermal behavior, PMH7 input power, radio and dock controls,
   display clocks, the native modeset (LVDS, VGA, dock DP/HDMI), backlight
   PWM, AHCI/HDA/USB/PCIe interrupts, the ExpressCard slot and emitted ACPI
   before OS use. Confirm a cold boot reaches the payload in one pass: the
   mainstage must not repeat the bootblock's soft-reset recovery.
7. Check both Penryn cores initialize, microcode is updated before PECI,
   and alternative SMRR is programmed/locked during SMM installation.
8. Test a short power-button press at the halt payload and OS shutdown after
   ACPI ownership. Check the thermal trips/passive-cooling processor references.
9. Check SMBIOS shows the EEPROM part number, serial and UUID, docking and
   undocking (button and power-loss paths), VT-d with the emitted DMAR, and
   on an X200t that the `WACF004` digitizer appears.
10. Confirm Linux `thinkpad_acpi` loads without `force_load`, Fn-F2/Fn-F3 events
   arrive, and both batteries report cycle count. Test charge thresholds and
   readback using the X200 `0x24/0x25` backend; the reusable H8 driver retains
   the `0xb0` backend for later ThinkPads.

Host validation commands:

```sh
cargo test -p fstart-arch --features x86_64
cargo test -p fstart-driver-intel --features gm45,ich9,acpi,display,spi
cargo test -p fstart-driver-usbdebug
cargo test -p fstart-platform-intel --features gm45-ich9,host
cargo test -p fstart-driver-lenovo --features fstart-arch/x86_64,acpi
```

These tests do not establish successful training, enumeration, firmware boot,
SMM protection or OS compatibility on an X200.
