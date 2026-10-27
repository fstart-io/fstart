# AMD Ethanol-X

Experimental **Milan / two-socket** bring-up port. Rome is not enabled: the
available firmware is Milan-only. Native SP3 platform code owns topology,
xGMI-related silicon work, memory, MP, SMM and ACPI; this crate supplies the
reference lane map, GPIO and console/KCS wiring.

## Bench

- Two EPYC 7643, CPUID `0x00a00f11`, 48 cores / 96 threads each.
- Vendor BIOS `RXM100CB`, MilanPI-SP3 `0.0.9.0`.
- ASPEED AST2500, Insyde BMC `03.55.37.0008`.
- Console: FCH UART1, MMIO `0xfedca000`, byte access/four-byte stride,
  48 MHz input, 115200 baud.
- Vendor PCI root buses: `00/20/40/60/80/a0/c0/e0`; BMC behind `60:05.2`.

The FCH UART1 MMIO console is hardware-verified. Native bring-up reports
256 GiB RAM and completes MP initialization of all 192 CPUs. It then halts
at socket 0's SMU DC BTC calibration (message `0x0c`, response `0xff`). No OS
or memory test has passed yet. The initial configuration does not implement
strap-driven slot bifurcation, the board's I2C clock/GPIO expanders, or
hotplug. Do not assume external slots match vendor Setup.
The current SP3 MP implementation also requires xAPIC: configurations above
254 threads halt rather than attempting x2APIC bring-up.

## Build and flash banks

The BMC exports a **64 MiB readout**, while SMBIOS reports a 16 MiB BIOS.
The readout contains an image at `[0,16 MiB)`, erased bytes at `[16,48 MiB)`,
and another image at `[48,64 MiB)`. Vendor UEFI reads through ROM2 match the
**lower** image (checked against differing NVRAM bytes). The export size
alone does not establish the physical flash-chip or chip-select geometry.

The current SP3 image composer accepts 16/32 MiB inputs. Build against the
**lower 16 MiB slice**, not the complete dump:

```sh
cargo fbuild assemble --board amd-ethanolx --release \
  --image-config /absolute/path/to/ethanolx-low.bin
```

The resulting `amd-ethanolx.rom` is one **16 MiB boot-bank image**, NOT a
complete flash image. **Do not use the BMC's Non-combo mode to preserve
the second vendor copy:** testing showed it mirrors a 16 MiB input into
both the lower and upper BIOS regions, leaving the middle 32 MiB erased.
A 64 MiB Combo-mode attempt left the same mirrored contents unchanged.
Use verified external backups for recovery, not an on-flash vendor copy.
The mirrored layout was independently checked against the built image
and original erased gap in two complete readbacks.

For a programmer confirmed to address the exported window, replace the
first 16 MiB of the verified readout and preserve the other 48 MiB
byte-for-byte. Never pad over the other vendor image with erased bytes.
Obtain independent matching reads and establish recovery before writing.
An upload acknowledgement or `OK` is not proof of a correct write; verify
the complete readout before booting.

## Recovered board data

Source: the bench's own RXM100CB BIOS, not a Tyan donor.
`AmdCpmOemInitPeim`, GUID `7b8f8199-42c7-db87-307e-2892f3849538`,
TE SHA-256 `bfc0f63a4ba5664c…`:

- Live CPM list at `0xffd68a54`, installed by `0xffd67684`.
- `$A10` at `0xffd6e3e4`: GPIO 89, setting `0x0988`, then terminator.
- `$A26` at `0xffd68d74`, `$A27` at `0xffd6bb1c`, descriptor stride
  `0x1b0`: socket-0 P0/P1/P2/P3, socket-1 P1 SATA and P3 PCIe.
- Runtime patcher `0xffd6814f` adds the BMC WAFL lane 129 when needed and
  rebuilds slot descriptors from straps/PCDs. The bring-up map includes the
  observed BMC link but does not reproduce those slot options.

The console follows RXM100CB `AmdIdsDebugPrintPei`, GUID
`239e812c-ad4a-4d5a-8bb9-169131c344bf`, TE SHA-256 `8c36a0cc5a598aa6…`:
its default path uses FCH UART1 MMIO, software reset plus about 100 us
settle, divisor 26 and UART pad function 0 (`0xffd02c28`). The shared FCH
UART driver owns clock/power/pin/reset setup; the standard 16550 driver
sets framing and baud. Live FCH inventory supplies its ACPI description.
No assumption is made that Insyde enables the newer OpenBMC LPC VUART.
