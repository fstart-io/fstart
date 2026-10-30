# Vilboz Linux diagnostics

## Scope

The Linux payload runs entirely from an embedded initramfs. It must not mount or
write the Chromebook's eMMC. Keep a full-chip recovery image outside the build
tree, verify two reads before programming, and compare a full readback before
booting each test image.

The work is based on `vbios/rust` plus the 4 MiB reset-image reservation
(`e1ea98dc`). The reference is the native Picasso implementation in the
`../coreboot_picasso` jj workspace (`26618924`).

## Concrete differences

### WLAN power/reset

Coreboot's `baseboard/dalboz/gpio.c:baseboard_pcie_gpio_configure()` runs a
revision-dependent WLAN power/reset sequence before training PCIe. The fstart
board had the same sequence in `vilboz::wifi_power_reset()`, but never called it.
Neither the PCIe GPIO table nor the RAM-stage table initializes GPIO29/42.

The board now calls the existing sequence after reading CBI and before DXIO:
assert GPIO29 (WLAN auxiliary reset), enable power on GPIO42 with the correct
board-revision polarity, wait 50 ms, then release reset. Older revisions also
power-cycle the active-high rail for 10 ms. Errors from GPIO writes propagate
instead of being silently reported as success.

### eMMC IRQ5

Coreboot's `soc/amd/picasso/acpi/mmio.asl` describes `AMDI0040` with a level-low,
exclusive interrupt. Its board routing assigns that interrupt to GSI 5.
Fstart routed it to the same GSI but described it with `GenericAcpi`, whose
interrupt descriptor is level-high. This programs the IOAPIC with the wrong
polarity: an inactive eMMC interrupt can look asserted, while actual command
completions are not handled correctly.

Generic device metadata now supports explicit trigger/polarity/sharing flags.
Picasso supplies level-low GSI 5 and the non-removable `CARD` child used by
coreboot. The interrupt number comes from the same routing table that programs
PCI_INTR, not a duplicated literal. Default attributes remain level-high and
exclusive for existing generic devices. The same host inventory is still passed
to CrabEFI: fixing AML must not remove UEFI's eMMC boot-device discovery.

### SCI routing

Coreboot programs PCI_INTR source `0x10` to IRQ9. The native Sandstone table did
not program this source at all, despite the FADT/MADT advertising SCI on GSI 9.
After correcting eMMC, Linux can freeze its tasks successfully, but the initial
RTC suspend test does not return. The route is now explicitly programmed, using
the same `SCI_IRQ` constant as the ACPI tables, rather than trusting reset or
previous-firmware state. With the route present, the traced hardware test
completes an initial sysfs suspend plus three successful `rtcwake` cycles.
SCI IRQ9 advances from zero to four interrupts, while eMMC IRQ5 continues to
handle commands after resume.

### Suspend is not just AML

The current AMD noncar flow is a cold-boot flow. It does **not** implement S3
resume, and its DSDT deliberately does not contain `_S3`. Linux `mem` must not be
reported as validated S3 solely because a suspend-to-idle cycle succeeds. Check
`/sys/power/mem_sleep` to distinguish `s2idle` from `deep`.

Coreboot's native Picasso S3 implementation also contains mechanisms not yet
wired into fstart:

- a one-time S3 resume-EIP MSR write and a protected-mode resume entry;
- persistent APOB/topology and ACPI/FACS allocations, recovered without clearing
  the suspended OS's tables;
- a resume-specific silicon path, not cold DXIO/DF/SMU initialization or PCI
  resource reassignment;
- PSP SxInfo from the registered SMRAM buffer and SMU S3-entry notification;
- SMM/CPU restoration, board sleep GPIO/EC policy, and a final jump through the
  existing OS wake vector.

Adding `_S3` without those mechanisms would advertise a destructive resume path.
The generic AMD DSDT also advertises S1 although the current FCH SMI sleep handler
ignores S0/S1 requests; that is separate from `mem`/suspend-to-idle.

## Wider ACPI comparison

| Interface | fstart | coreboot native Picasso/Zork |
| --- | --- | --- |
| FADT, FACS, MADT, MCFG, HPET, IVRS | Present | Present |
| PCI root windows and APIC `_PRT` | Present | Present, also PIC routing |
| GPIO and used I2C controllers/devices | Present | Present, plus AOAC power methods |
| FCH UART OS discovery | Missing (early console only) | Present |
| eMMC host/card | Corrected here | Present |
| EC keyboard | Present | Present |
| EC ACPI operation region, event queries/GPE dispatch | Missing | Present |
| AC adapter, battery, lid and EC thermal integration | Missing | Present |
| CPU idle/performance policy and platform-specific ALIB | Incomplete | Present |
| S3 entry/resume | Missing | Present |

This is a list of interfaces to port deliberately, not a recommendation to copy
the entire coreboot namespace or its OS-specific ChromeOS policy.

## Reproducing the firmware build

```sh
cargo fbuild assemble --board google-vilboz --release --payload linux \
  --kernel /path/to/bzImage-with-embedded-initramfs \
  --linux-kernel-load-addr 0x04000000 \
  --linux-bootargs 'earlycon=uart8250,mmio32,0xfedc9000 console=uart8250,mmio32,0xfedc9000 no_console_suspend loglevel=7 printk.devkmsg=on panic=-1' \
  --donor /path/to/verified-vilboz-backup.rom
```

Use a kernel with ACPI, PCI, SMP, suspend/freezer, RTC CMOS, MMC/SDHCI ACPI,
DesignWare I2C and AMD pinctrl built in. Enable `BLOCK` and `MMC_BLOCK` for
read-only block-I/O tests. The payload and authenticated video ROMs must fit the
fixed FFS window; the 4 MiB reset image does not enlarge that window.

Early console output is not a userspace serial console. Until the FCH UART has
an OS-facing ACPI node, this payload logs through `/dev/kmsg`. Use
`printk.devkmsg=on` to avoid rate-limiting away its snapshots, and keep individual
writes at most 1024 bytes. An unavailable `/dev/console` must not discard the
diagnostic program's output.

Capture the initial kernel log, PCI vendor/device/class/resource attributes,
`/proc/interrupts`, `/proc/driver/rtc`, `/sys/power/{state,mem_sleep}`, and
`/sys/power/suspend_stats/last_failed_*`. Run `rtcwake -s 5 -m mem` as root from
the initramfs, repeat it, and confirm post-resume eMMC reads and PCI enumeration.
Do not turn off a suspected device or mask IRQ5 to call the interrupt problem
fixed.

## Validation status

Software: 24 AMD platform tests, 323 AMD driver tests, 112 ACPI unit tests
(and its integration/doc tests), 13 Lenovo ACPI/driver tests, and seven board
tests passed. Changed Rust files have no LSP warnings/errors. Platform-package
clippy with `--no-deps -D warnings` passes. Strict ACPI-package clippy is blocked
by the existing `chunks_exact_to_as_chunks` lint in `wake.rs`; dependency-wide
clippy also reports existing warnings outside these changes.

The unchanged firmware reproduces eMMC command-interrupt timeouts and a busy
`events_freezable` workqueue. All three actual `rtcwake -s 5 -m mem` calls
returned `Resource busy` after about 20.12 seconds (exit status 1). Linux reports
`[s2idle] shallow`, not `deep`, and no WLAN PCI endpoint is enumerated. This test
has not reproduced a genuine kernel panic or an IRQ5-disable warning: those must
not be conflated with the observed timeout/freezer failure.

The first fixed-image boot enumerates Realtek WLAN `10ec:c822` at `01:00.0`,
initializes `MMC32G` as a 29.1 GiB eMMC, and handles eMMC interrupts on GSI 5.
The previous MMC command timeouts disappear, and the suspend freezer completes.
RTC wake does not complete in this first test. Adding the SCI-routing fix then
passes three actual `rtcwake -s 5 -m mem` calls (exit 0; approximately 7.8–8.0 s
including verbose callback tracing) and a post-resume 4096-byte eMMC read. No
kernel panic, oops, IRQ-disable warning, MMC timeout or freezer failure occurs.
Native coreboot was also built and flashed with the same Linux kernel and
embedded initramfs. It enumerates the same WLAN/eMMC, an EC, UART and thermal
zone, and reports `s2idle shallow [deep]`. Its initial `mem` test enters the
`deep` path and completes the freezer stage, but no resume is observed within
the 180-second capture. Thus this experiment does **not** validate a working
coreboot S3 cycle either; it does establish the ACPI/silicon-initialization
comparison against a real reference boot.

The untraced fstart image passes three further `rtcwake` cycles in
5.784/5.991/5.984 seconds, followed by a 4096-byte eMMC read. IRQ5 advances from
77 to 133 serviced commands and IRQ9 from zero to four RTC wakes (including the
initial sysfs suspend). A Cr50 `ecrst pulse` reset-path retest passes another
three cycles in 5.793/5.989/5.982 seconds and the same eMMC read. WLAN remains
enumerated after resume in both boots. A corrected `uefi-ui` image also builds
successfully; its device inventory still includes eMMC. Hardware validation here
uses the Linux payload, not the installed OS.

Local images, kernel configuration, initramfs source, build logs and recovery
reads are under `.local/vilboz-linux/` in the `fstart-vilboz-linux` workspace:

- `clean-linux.rom`: untraced Linux hardware-test image.
- `corrected-uefi-ui.rom`: corrected graphical UEFI image, build-tested only.
- `sci-result.json`, `clean-result.json`, `ecrst-result.json`: passing checks of
  the three traced/cold/reset captures, including all nine `rtcwake` calls.
- `coreboot-boot.log`: reference boot and incomplete deep-sleep attempt.
- `recovery-read{1,2}.rom`: matching pre-test recovery images.

An independent backup is retained outside the checkout at
`~/.local/share/firmware-backups/vilboz/linux-acpi-20261030/pre-test.rom`.
The pre-test image's SHA-256 is
`db29c18a6192218e0389ebd3e8be995c6f1087ab6836bdb8ae9829f0765bbc76`;
both fresh recovery reads match it. At the end of testing this exact image was
restored and matched by an explicit full-chip readback. EC `powerinfo` confirmed
G3 afterward, with no SPI/UART handles left open. No target filesystem was
mounted or written.
