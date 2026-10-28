# Native AMD Picasso / Google Vilboz

Native (no x86 FSP-M/S) port of AMD Picasso and Google Zork "Vilboz".
The PSP/ABL still trains DRAM and supplies the APOB before releasing x86.

## Code layout

- `crates/driver-amd`: reusable CCX, DF, NBIO, DXIO, SMU v10 and Sandstone
  FCH drivers. `nbio::picasso::gfx` prepares the GPU memory map and ATOM
  system information; it does not set a display mode.
- `crates/driver-vbios`: shared x86 option-ROM emulator and VBE mode
  selection, returning the common framebuffer handoff for GOP.
- `crates/platform-amd/src/picasso`: Picasso's configuration and timepoints
  on the shared [noncar flow](amd-noncar.md), including MP, SMM, PCI,
  ACPI and payload handoff.
- `boards/google/zork`: Dalboz wiring, Vilboz GPIO/PCIe/USB/display policy,
  ChromeEC CBI discovery and the FCH SMM handler. Vilboz, Vilboz14 and
  Vilboz360 share this board; EC CBI supplies revision/SKU differences.
- `crates/image-build`: the native flash image: the PSP partition packed
  from the AMD blobs, the APCB instances generated from the board's
  template and SPDs, fstart's PSP user application, the reset image and
  the FFS with the microcode and the VBIOS files. The shared
  [AMD format library](amd-image-format.md) reads and writes the PSP/BIOS
  directories and their checksums.

## Cold boot

1. Enable ECAM and ACPI MMIO, power the early FCH devices and initialize
   the NS16550 console.
2. Read CPU identity, TOP_MEM/TOM2 and the APOB memory map. Reserve the
   firmware and TSEG; install AMD MTRRs and authenticate the FFS.
3. Set up DF/NBIO/PSP, early CPU policy, LPC/SPI/SMBus and eSPI. Read CBI,
   apply the board GPIOs and select its SMU power/eMMC policy.
4. Train DXIO, set PCIe power policy, initialize the FCH clocks, legacy
   routing, USB and eMMC. Keep all SMU/DXIO DMA buffers reserved.
5. Release the APs through the SMU. Every CPU loads microcode and runs
   CCX initialization. Notify the SMU, prepare GPU UMA/system information,
   and finish NBIO initialization.
6. Relocate and lock SMM on every CPU, test a permanent SMI, and send the
   SMM information to the PSP.
7. Allocate PCI resources and install WC for the assigned prefetchable
   VGA apertures using the shared x86 cache-policy updater. All online CPUs
   rendezvous before cache/MTRR changes; readback verifies the result on
   every CPU. Existing AMD RAM/ROM MTRRs, fixed DRAM routing and TOM2WB
   remain unchanged. Intel uses the same aperture selection before MP.
   Configure IOAPICs and generate ACPI tables. The enabled eMMC's resources
   feed both ACPI and CrabEFI discovery.
8. Forward the legacy VGA apertures, run the authenticated VBIOS, read
   EDID and set a validated linear framebuffer mode. Raven2 uses
   `pci1002,15dd.rom` even when its PCI ID remains `1002:15d8`, matching
   coreboot's silicon-specific ROM selection. This is the option-ROM/VBE
   display path, not a new native Radeon modesetter.
9. Lock DF registers, map RAM for the payload, park the APs, send PSP
   BootDone and enter CrabEFI (or the selected Linux payload).

There is no early-init checkpoint in this flow. Any failed required step
stops with an error rather than falling back to FSP.

## DRAM and flash layout

| Use | DRAM range |
| --- | --- |
| APOB | `02001000..02011000` |
| PSP shared memory | `02011000..02019000` |
| Reset page tables | `02020000..02026000` |
| Data/BSS and persistent DMA | `02030000..02200000` |
| Firmware heap | `02200000..02a00000` |
| BSP stack | `02f00000..03000000` |
| PSP-loaded reset image | `03000000..03200000` |
| Reset vector | `031ffff0` |

`02000000..03200000`, the AP bootstrap page at `0x8000`, and TSEG remain
reserved through payload handoff. APs enter the same reset vector and are
redirected to the architectural MP trampoline; the PSP's image is not patched.

fstart lays out the whole 16 MiB flash:

| Flash | Use |
| --- | --- |
| `000000..980000` | erased |
| `980000..b80000` | reset image, AMD-compressed; the PSP inflates it |
| `b80000..e1f000` | the authenticated FFS: payload, microcode, VBIOS |
| `e20000..1000000` | EFS, PSP and BIOS directories, AMD blobs, APCBs |

The PSP of Vilboz's Chromebook part starts the x86 cores only after the
user application of PSP entry 0x52 (the verstage slot) ran; without one it
never releases them. fstart packs its own, which only exits
(`image-build::amd::psp_app`).

## Building

The image is packed from a blob directory ([AMD firmware
blobs](amd-image-format.md#firmware-blobs)): Picasso's files as
`crates/platform-amd/Cargo.toml` names them, and Vilboz's 13 APCB instances
as `boards/google/zork/Cargo.toml` asks for them. Fill it from the board's
own flash image, or use an `amd_blobs` checkout and add the APCBs:

```sh
cargo fbuild blobs --board google-vilboz --from vilboz-backup.rom --out blobs/
cargo fbuild assemble --board google-vilboz --release --payload uefi-ui --blobs blobs/
# or in one step:
cargo fbuild assemble --board google-vilboz --release --payload uefi-ui --donor vilboz-backup.rom
```

This emits `google-vilboz.rom`, the whole flash. Clang builds the vendored
x86emu; the Nix shell supplies it. Use `uefi` instead of `uefi-ui` for GOP
without the graphical boot-manager feature.

`uefi-ui` also hands the board's I2C devices to CrabEFI as peripherals:
fstart sets up each DesignWare controller and its pads, and CrabEFI picks a
driver by ACPI ID. On I2C2 these are the two touchpads Vilboz may be fitted
with, ELAN0000 at `0x15` and a HID-over-I2C Synaptics at `0x2c`, both on
active-low GPIO 9; the one that is absent fails to initialize and is
skipped. They provide movement and physical buttons, not gestures or
tap-to-click, and sleep at ExitBootServices. Plain `uefi` hands over the
Cr50 on I2C3 (`GOOG0005` at `0x50`, ready interrupt latched on GPIO 3),
which backs CrabEFI's measured boot, and the eMMC controller. This does not
yet add OS-facing I2C/touchpad ACPI nodes.

The generated development signing key provides image integrity, not
production rollback protection.

## Hardware validation

Vilboz revision 4, FW_CONFIG `0x2099200`, 4 GiB DRAM, booting the native
image (nothing of the Chromebook firmware left in the flash):

- Four CPUs initialized with patch `0x08200105`, zero CPU-init errors.
- All four SMM entries relocated/locked and a permanent SMI returned.
- PCI enumeration, ACPI, PSP LockDFReg/BootDone and AP parking succeeded.
- WC verified on all four CPUs for the framebuffer aperture
  `e0000000..f0000000` and prefetchable GPU memory `d0a00000..d0c00000`;
  non-prefetchable GPU registers remain UC.
- VBIOS POST completed, VBE mode `0x1d4` set 1366x768 at `0xe0000000`,
  32 bpp, 1408-pixel stride. CrabEFI installed GOP; panel output was
  confirmed on the board.
- ELAN initialization completed and reported a 3282x1793 sensor; the
  CrabEFI cursor followed it and its button clicked. The absent Synaptics
  alternative NACKed its address and was skipped.
- The Cr50 on I2C3 (DID_VID `0x00281ae0`) backed CrabEFI's TCG2 protocol
  with SHA-1/256/384/512 banks, using its latched ready interrupt, and
  measured PCR 0 and 7.
- An `R` press injected through the EC keyboard matrix reset the system
  from CrabEFI's no-media screen.
- CrabEFI initialized and its SDHCI/xHCI drivers ran; onboard eMMC was
  registered and scanned for partitions. No bootable media was found,
  matching the original native coreboot/CrabEFI baseline.

The EC independently confirmed the CBI values. Optional SSFC is absent;
its absence preserves the valid low FW_CONFIG word, as in coreboot.
Backup, intermediate images and serial/flash/test logs are local artifacts
under `.local/vilboz/`, not repository inputs.

An intermittent AP-release exception has also occurred on EC-only resets,
including before the WC update runs. A confirmed G3 boot passed WC/GOP
validation; the separate reset issue is not yet root-caused.

A direct Linux 7.2.0-rc6 payload with a built-in test initramfs reached
userspace with CPUs `0-3` online. The kernel had suspend and ACPI sleep
support enabled, but reported ACPI S0/S1/S5 and only `[s2idle] shallow` in
`/sys/power/mem_sleep`: firmware does not expose S3/deep sleep. This was a
flash-resident Linux payload test, not a disk boot through CrabEFI.

Remaining limitations: no S3 resume or APOB-NV cache persistence;
CrabEFI variables are volatile. No successful S3 resume is claimed.
