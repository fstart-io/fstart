# AMD Ethanol-X

AMD SP3 reference board, two sockets, EPYC 7003 (Milan) only: the
available vendor firmware (RXM100CB, MilanPI-SP3 0.0.9.0) is Milan-only.
Status: **experimental**. Boots RAM-only Linux on all 192 CPUs of two
EPYC 7643 with two NUMA nodes; CrabEFI reaches its boot manager.

The SP3 platform owns topology, memory, MP, SMM and ACPI. This crate is
the board's data: identity and strap detection over I2C, the PCIe lane
map, GPIO, SATA policy, the FCH UART1 console and the BMC's IPMI KCS.

| Module | What |
|---|---|
| `lib.rs` | the `fstart_platform_sp3::board::Board` implementation: policy, console, GPIO, ACPI |
| `i2c` | board ID EEPROM, PCIe strap/routing expanders, redriver programming |
| `dxio` | revision `0x30` lane map from the PCIe straps |
| `smm` | the SMM image's handler binding (SMM unit only) |

## Build

```sh
cargo fbuild assemble --board amd-ethanolx --release
cargo fbuild assemble --board amd-ethanolx --release --payload uefi
```

The vendor ROM is the board's own flash (`vendor/README.md`). The BMC
exports a 64 MiB window: the boot bank at `[0, 16 MiB)`, erased bytes at
`[16, 48 MiB)` and a second copy at `[48, 64 MiB)`. Its BIOS update
(non-combo mode) writes a 16 MiB upload to **both** the lower and the
upper bank and leaves the middle alone; this was confirmed by complete
readbacks. The board declares that mirror (`FLASH_MIRRORS`), so with the
64 MiB readout as vendor ROM the build produces:

- `amd-ethanolx.rom`: the full 64 MiB flash image, bank at 0 and 48 MiB,
  the readout's middle kept byte for byte, for an external programmer;
- `amd-ethanolx.bank.rom`: the 16 MiB bank, for the BMC BIOS update.

With only the lower 16 MiB as vendor ROM, `amd-ethanolx.rom` is the bank.
The bank layout: vendor EFS and directories to `0x893000`, FFS from
`0x893000`, reset image at `0xde0000`.

APCB configuration is built from semantic policy in `apcb/milan.toml` and
framework Rust firmware-family models, not copied as an opaque blob. The initial settings reproduce RXM100CB exactly;
other APCB contents are rejected by donor hashes until reviewed. See
`docs/amd-apcb.md`. This source-generation path has host reconstruction tests,
not a repeated hardware qualification.

There is no on-flash vendor fallback after an update: both banks hold the
new image. Keep verified external readouts for recovery, verify the
complete readout after a write (an upload `OK` is not proof), and do not
use combo mode (a 64 MiB combo upload did not restore a vendor bank).
The export size does not establish the physical flash-chip geometry.

## Board revision and PCIe

Detection is read-only: the M24LC128 on I2C5 (`0xfedcb000`, not the
linear `0xfedc7000`) at `0x50`, byte `0x10`, is the board ID; the PCA9535A
on I2C0 at `0x20` reports the slot straps. Only revision `0x30` is
supported; others halt before any output changes. For `0x30` the board
then drives PCA9535A `0x22` P0.1 low (vendor order, with readback) and
re-reads the straps. Device errors halt rather than selecting a default
lane map.

Socket-0 strap nibbles select P0/P1/P2/P3 at shifts `0/8/12/4`:

- bit 3 clear: reversed x16 with the vendor slot number;
- `0xa`/`0xc`: quad x4 with PCIe-native Enterprise SSD hotplug;
- `0xb`/`0xd`: x8 + x8; `0xe`/`0xf`: forward x16;
- P0/P1 `0x8`: SATA on lanes 0–3, Ethernet on 4–7 and x8 PCIe on 8–15;
  `0x9` omits the PCIe link. P2/P3 `0x8`/`0x9` fall back to reversed x16.

Socket 1 has fixed SATA on P1 (AHCI controller 1), x16 PCIe on P3 and four
x4 NVMe hotplug ports on P0. The BMC uses WAFL lane 129 at Gen1. The NBIO
fuse reduced-I/O signature (`(fuse1 & 0x5380) == 0x5380`) omits socket-0
P1 and socket-1 P0, as the vendor does.

Selector `0xa` programs four DS125BR401A redrivers behind the matching
PCA9545A (`0x70`–`0x73`, channel 1) with the vendor tuning; P0/P1 `0x8`
uses the vendor's three-chip SATA/Ethernet profile at `0x58`–`0x5a`. The
mux is deselected even after a failed write.

Milan has no integrated xGbE (PPR 55898 lists no Ethernet complex; the
vendor's xGbE PCDs `0x148`–`0x14f` are all false), so Ethernet
descriptors of the mixed layouts are dropped with a warning.

The board selects the vendor's pre-training PCIe capabilities (10-bit
tags, Data Link Feature Exchange: PCDs `0x1c2`/`0x16d`/`0x16e`).

## Sources

All board data comes from the bench's own RXM100CB image, not a donor:

- `AmdCpmOemInitPeim` (`7b8f8199-42c7-db87-307e-2892f3849538`): CPM `$A10`
  at `0xffd6e3e4` (GPIO 89, setting `0x0988`), `$A26`/`$A27` lane
  descriptors at `0xffd68d74`/`0xffd6bb1c` (stride `0x1b0`), runtime
  patcher `0xffd6814f`, group builder `0xffd67b60`, redriver tables
  `0xffd72994` (PCIe) and `0xffd70fd4` (SATA/Ethernet).
- `AmdIdsDebugPrintPei` (`239e812c-ad4a-4d5a-8bb9-169131c344bf`): the
  console, FCH UART1 MMIO `0xfedca000`, 48 MHz, byte access with four-byte
  stride, 115200 baud.

## Validated on hardware

Two EPYC 7643 (CPUID `0x00a00f11`), 256 GiB, AST2500 BMC. The 64 MiB
image built from this tree, written through the BMC and read back in
full, boots RAM-only Linux with:

- all 192 CPUs on two NUMA nodes (SRAT covers both sockets), a memory
  test pinned to each socket and zero EDAC errors, ACPI power-off;
- SMBIOS decoded by dmidecode: 2 packages, 96 cores, 192 threads, 16 of
  16 DIMMs, 262144 MiB;
- a KVM guest run, IOMMU with interrupt remapping (59 groups);
- with straps `0x0000`, `pciehp` on `e0:03.1`–`e0:03.4`.

Earlier runs of the port also covered the FCH UART1 console, four xHCI
controllers with a USB 3 disk read, AHCI on four controllers with 17
ports including socket 1's eight (no media attached, so no SATA link),
and CrabEFI up to its boot manager.

Socket 1's SMU rejects the CPPC table (`CppcSetTable` answers 0xff), so
only socket 0 gets `_CPC` and Linux leaves amd_pstate off.

Not validated: other strap settings, the redriver paths, populated slots
and hot insertion, SATA links, an installed OS from disk, persistent
CrabEFI variables (there is no backend). More than 254 threads needs x2APIC,
which the SP3 MP code does not do yet.
