# Google Peppy — Acer C720 Chromebook

**Build- and host-tested, not boot-tested on physical hardware.** This is the
Slippy-family Haswell ULT / Lynx Point-LP board with soldered DDR3L, an LPC
ChromeEC and an 8 MiB AP flash. It includes the C720P touchscreen description.
It is not a build for Falco, Leon, Wolf or a later Acer Chromebook.

## Build and assemble

From the repository root, with the pinned toolchain, ACPICA (`iasl` and
`acpiexec`), and `intel-microcode/intel-ucode/06-45-01` from the pinned Intel
microcode submodule available:

```sh
cargo fbuild build-boards google-peppy                 # release stages + board host tests
cargo fbuild assemble --board google-peppy --release   # default halt payload
cargo fbuild assemble --board google-peppy --release --payload uefi-ui \
  --debug flash-console --log-level debug
cargo fbuild options google-peppy
```

`assemble` prints the output location under `target/fstart-build/google-peppy/`.
`google-peppy-8m.pflash` is the chip-sized image; `google-peppy.ffs` is **not**
a full flash image. The BIOS contains the reset/CAR bootblock (including the
four trusted SPD images), microcode, postcar (`postcar_bin!`), ramstage, the
SMM image and the verified `data.vbt`. The payload depends on the selection. Use
`--signing-key` for your own Ed25519 key; the default is an ephemeral
development key. No MRC/PEI binary, VGA ROM, vendor BIOS or replacement EC
firmware is needed to build.

## Architecture highlights

- **Decoupled northbridge/southbridge:** `Haswell` receives Lynx Point-LP
  apertures and ME9 callbacks via `HaswellPch` in `fstart-platform-intel`
  rather than coupling northbridge and southbridge drivers directly.
- **Native Haswell display bring-up (`gma::Haswell`):** `HaswellIgdConfig`
  configures eDP (DDI-A, 1366x768 fallback, panel power sequencer delays and
  200 Hz backlight PWM) and HDMI1 (DDI-B, `Port::HdmiA`), with `data.vbt`
  packaged as a verified FFS asset for OpRegion 2.0 and modeset.
- **Store-backed TPM S3 state (`fstart_store::tag::TPM_STATE`):** cold-boot
  TPM presence, DID:VID, family, revision and IRQ routing are saved in the
  RAM-backed firmware store rather than CMOS, so cold boots do not disturb
  ChromeOS VBNV or CMOS checksums while S3 resume strictly verifies the
  retained TPM population.
- **Cold-boot and ACPI EC SMI handoff:** on cold boot (`SCI_EN = 0`), the
  one-time SMM handoff arms legacy EC lid-close SMI poweroff in payload mode;
  `APM_CNT_ACPI_ENABLE` transfers ownership to SCI, `APM_CNT_ACPI_DISABLE`
  re-arms legacy SMI, and S3 resume (`SCI_EN = 1`) preserves OS SCI ownership.
- **Minimal postcar:** `src/bin/postcar.rs` uses `fstart_platform_intel::postcar_bin!`.

## Flash only the BIOS region

| Region | Chip offsets (end exclusive) | Source |
| --- | --- | --- |
| IFD | `0x000000..0x001000` | Retain the machine's descriptor |
| ME9 | `0x001000..0x200000` | Retain the machine's installed ME |
| BIOS | `0x200000..0x800000` | Replace with fstart BIOS |

These bounds are the **expected** `slippy/chromeos.fmd` geometry, not a
measurement of your chip. There is no GbE region. **The first 2 MiB of the
assembled file are `0xff`, not a descriptor and ME. Do not write it to the
whole chip.**

1. Identify the AP flash and correct programmer voltage; don't program the EC
   flash. With the AP unpowered as required by your programmer, make two
   complete reads, compare them and keep a verified original off-machine.
   Keep an external recovery programmer available.
2. `ifdtool -d original.rom` must show an 8 MiB part and the exact enabled
   regions above. **Stop on a mismatch.** Archive the entire BIOS too: this
   replaces ChromeOS RO and both RW slots, GBB, VPD, recovery data and
   bootloader. fstart does not migrate those records. Respect flash and board
   write protection; do not change descriptor permissions to fit the image.
3. With a supported programmer and a verified matching descriptor, e.g.
   `flashprog -p "$PROGRAMMER" --ifd -i bios -w "$IMAGE"` using the **full
   8 MiB** `.pflash` file. Read back and compare the BIOS with the new image
   and the first 2 MiB with your backup. If preparing a full-chip file offline,
   start with the verified backup and replace **only** its BIOS slice.

The BIOS starts with a 128 KiB training journal (`0x200000..0x220000`).
`--debug flash-console` adds an erased 256 KiB log (`0x220000..0x260000`)
immediately after it. Both need an accessible, supported SPI chip and writable
BIOS extents. Don't transplant a training journal between boards.

## Where the board facts came from

Relative to coreboot `src/mainboard/google/slippy`:

| Reference | Board implementation |
| --- | --- |
| `Kconfig`, `chromeos.fmd`, `devicetree.cb`, `romstage.c` | `src/config.rs` flash, GMA policy, and assets; [`src/slippy.rs`](src/slippy.rs) family LPC windows, DxxIP/DxxIR routing, SCI GPIO36, SMI GPIO34, Serial IO and double refresh |
| `gma-mainboard.ads`, `variants/peppy/{overridetree.cb,romstage.c}` | `config.rs`: eDP + HDMI1 (`Port::HdmiA`), panel delays/200 Hz PWM, RP1/2 wiring, ASPM, SATA DTLE, ME clock outputs, eight USB2/four USB3 entries; `spd.rs`: straps and revision |
| `variants/peppy/{Makefile.mk,spd/*.spd.hex}` | `spd/*.spd`: 256 bytes per part, whitespace/comments removed, byte order unchanged. Index low two bits select Micron, Hynix AFR, Elpida, Hynix CFR in that order |
| `variants/peppy/{gpio.c,hda_verb.c}` | `pads.rs`: all 95 pads, not the C terminator; `hda.rs`: ALC283 double reset, programmed subvendor `1179:0670`, pins, Realtek coefficients and beep verbs |
| `data.vbt` | `data.vbt`, copied unchanged from Slippy; verified from FFS for OpRegion 2.0 and native `gma::Haswell` modeset |
| `ec.c`, `ec.h`, `smihandler.c`, `acpi_tables.c` | `slippy.rs`, `smm.rs`, stage mains: mask handoff, cold-boot and ACPI-disable lid-close S5 SMI, charging in S3/not S4–5, S3/5 wake events, sleep rail control |
| `dsdt.asl`, `acpi/*`, `thermal.h`, `onboard.h`, variant `acpi/*`, `mainboard.c` | `acpi.rs`: LPC EC/lid/AC/battery, keyboard IRQ1, probed TIS TPM, Slippy thermal zone, Serial IO children, IRQ/wake resources, USB connector data, SMBIOS identity |
| `cmos.layout`, `cfr.c` | `options.rs` and the normal CMOS-backed fstart knobs, not the ChromeOS CMOS bit layout |

**Memory selection:** GPIO13/9/47 are the low/middle/high RAM-ID bits. For
EC board version zero (prototype), only ID 0 is single-channel. For **every
nonzero version**, IDs 0–3 are dual and 4–7 single. CH0 slot 0 is always
populated; CH1 slot 0 uses identical SPD bytes when populated. Both slot-1s
are empty. Firmware uses `GET_BOARD_VERSION`, **not CBI**. It selects linear LPC v3
or the v2 args interface at discovery, never by replaying a submitted command.
If the read fails, IDs 1–3 are unconditionally dual-channel. For ID 0 or 4–7
firmware warns, emits POST `df`, and assumes PROTO as coreboot does.

**OS devices and options:** `touchpad_type=Auto` emits Elan (`0x15`) and
Cypress (`0x67`) on I2C0, IRQ37 and wake GPIO12. A forced setting omits the
other device. Atmel (`0x4a`) on I2C1 uses IRQ38 and wake GPIO25;
`ambient_light` enables ISL29018 (`0x44`, IRQ35) on I2C1. `lte_ngff` enables
physical RP2 **before enumeration** and adds the SIM connector to PRT6.
Both options default off. The cold port mask and physical-to-PCI function map
are checked with the Serial IO BARs on S3. Unlike Peppy's `gpio.c`, GPIO51 is
routed to PIRQT so the advertised ALS interrupt is supplied when fitted.
The AML uses GPE `0x69` for the EC's WAKE# and GPIO36 for SCI. PECI sensor 0
drives the 95/99 °C passive/critical trips with a critical re-read and 12 W EC
throttle.

**Differences and limits:** board AML is composed into the DSDT instead of
using GNVS and a separate board SSDT. Firmware owns FIFO TPM startup: CLEAR on
cold boot, STATE on S3. Cold TPM identity and IRQ routing are retained in
`fstart_store::tag::TPM_STATE` (RAM firmware store); S3 compares it before
sending any command and cold-resets on mismatch. A cold startup or persistence
failure withholds S3. TPM 2 gets a TPM2 table; no measured boot or event log
is implied. There is no ChromeOS vboot/recovery, EC firmware sync, vboot
GPIO/trust handoff, ChromeOS VBNV, legacy VGA INT15, or EC wake/shutdown
ELOG/MKBP logging. Per-port GNVS `S3U0/1` and `S5U0/1` overrides are not
supported: both ports retain charging in S3 and disable charging in S4/S5.
Peppy has the i8042 keyboard and `\_SB.CREC`, not a CKSC/GOOG0007 child.
Unlike coreboot's `battery.asl`, the optional near-full `_BST` correction is
only for an idle battery on AC, never one charging or discharging.

**CMOS rollback:** If you modify menu options in fstart, the fstart option
window (`0x40..=0x7f`, including its checksum) overlaps the tail of ChromeOS
VBNV (`0x34..=0x43`) and coreboot's checksum at `0x7b..=0x7c`. Save CMOS as
well as AP flash before switching firmware.

## Verification status

Build- and host-tested only; no physical Acer C720/C720P hardware boot is
claimed here. Host tests (`cargo test -p fstart-board-google-peppy --lib`)
verify:

- Every RAM-ID / EC board revision combination (`spd.rs`, `tests.rs`)
- All 95 Peppy GPIO pads and ALC283 HDA verb tables (`tests.rs`)
- Peppy `HaswellIgdConfig`, `gma::Haswell` outputs (`Edp` + `HdmiA`), panel
  delays/PWM, and `data.vbt` header (`tests.rs`)
- `fstart_store::tag::TPM_STATE` round-trip and mismatch rejection (`fstart-platform-intel::tpm`)
- Cold-boot lid-close SMI arming, `ACPI_ENABLE`/`ACPI_DISABLE` transitions,
  S3 resume SCI preservation, and rogue handoff rejection (`fstart-driver-chromeec::handoff`)
- Full Haswell + Lynx Point + Peppy DSDT disassembly/recompile (`iasl`) and
  ACPI method execution (`acpiexec`) across three device-population policies (`tests.rs`)
