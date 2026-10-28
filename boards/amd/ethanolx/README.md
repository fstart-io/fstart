# AMD Ethanol-X

Experimental **Milan / two-socket** bring-up port. Rome is not enabled: the
available firmware is Milan-only. Native SP3 platform code owns topology,
xGMI-related silicon work, memory, MP, SMM and ACPI; this crate supplies the
board identity/strap policy, GPIO and console/KCS wiring.

## Bench

- Two EPYC 7643, CPUID `0x00a00f11`, 48 cores / 96 threads each.
- Vendor BIOS `RXM100CB`, MilanPI-SP3 `0.0.9.0`.
- ASPEED AST2500, Insyde BMC `03.55.37.0008`.
- Console: FCH UART1, MMIO `0xfedca000`, byte access/four-byte stride,
  48 MHz input, 115200 baud.
- Vendor PCI root buses: `00/20/40/60/80/a0/c0/e0`; BMC behind `60:05.2`.

The FCH UART1 MMIO console is hardware-verified. Native bring-up reports
256 GiB RAM and completes MP/SMU initialization on both sockets, SMM
relocation and locking on all 192 CPUs, PCI enumeration, two-domain NUMA
ACPI generation and handoff to the halt payload. RAM-only Linux boots all
192 CPUs across two NUMA nodes, passes a 32 MiB memory smoke test pinned
to each socket with zero EDAC errors, and powers off through ACPI.
CrabEFI (`--payload uefi`) initializes its EFI environment, imports the full
memory map and ACPI tables, initializes USB and AHCI, and reaches its boot
manager. No EFI application or OS was launched. Its pinned revision has a
64-device PCI enumeration limit; native enumeration now finds 157 functions,
so complete CrabEFI peripheral coverage is not validated. Variables are volatile:
this port supplies no persistent CrabEFI variable backend.

Revision `0x30` has vendor-derived strap bifurcation, GPIO/mux/redriver
sequencing and native NVMe hotplug policy. With measured straps `0x0000`,
Linux registers four `pciehp` controllers at `e0:03.1`–`e0:03.4`, exposing
empty downstream slots on buses `e1`–`e4`, and passes the CPU/NUMA/memory
smoke checks before clean shutdown. GPIO expander writes are readback-verified.
The muxed redriver path is host-tested but not exercised by these straps.
Populated slots, alternate strap settings and physical insertion/removal
remain unvalidated. Vendor Setup overrides are not implemented.
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
# CrabEFI payload, with the same vendor input:
cargo fbuild assemble --board amd-ethanolx --release --payload uefi \
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
  rebuilds slot descriptors from straps/PCDs. Native policy preserves the
  observed BMC link and socket-1 SATA/P3 links, and implements revision
  `0x30`'s socket-1 P0 quad-x4 native-hotplug template.

Read-only detection is hardware-verified: M24LC128 on I2C5 at `0x50`,
byte `0x10`, reports board ID `0x30`; PCA9535A on I2C0 at `0x20` reports
PCIe straps `0x0000`. I2C5 is at `0xfedcb000`, not the linear `0xfedc7000`.
Detection preserves power/pad routing and does not write EEPROM data.
Other board revisions halt before output changes. The supported revision
then drives PCA9535A `0x22` P0.1 low with preservation/readback, following
the vendor order, and re-reads the authoritative straps. The exact signal
name is not proven; no guessed clock-frequency programming is performed.
Device errors halt rather than silently selecting a default lane map.

The socket-0 strap nibbles select P0/P1/P2/P3 at shifts `0/8/12/4`:

- Bit 3 clear: reversed x16, retaining the vendor physical-slot mapping.
- `0xa`/`0xc`: quad x4, PCIe-native Enterprise SSD hotplug.
- `0xb`/`0xd`: x8+x8; `0xe`/`0xf`: forward x16 fallback.
- P0/P1 `0x8`: SATA on lanes 0–3, four Ethernet descriptors on 4–7,
  and an upper x8 PCIe link on 8–15. `0x9` omits the upper PCIe link.
  Both carry the recovered SATA PHY parameters also used by socket 1.
- P2/P3 `0x8`/`0x9`: ordinary reversed x16 fallback. The OEM enables
  mixed cards with flag `0x20` only on P0/P1; do not repurpose P2/P3.

The existing NBIO fuse reader supplies the vendor reduced-I/O condition
`(fuse1 & 0x5380) == 0x5380`, omitting socket-0 P1/socket-1 P0 when set.
This is not a socket-count test. Selector `0xa` programs four DS125BR401A
redrivers through the matching PCA9545A mux (`0x70`–`0x73`, channel mask 1),
using the board's recovered tuning bytes. The mux is deselected even after
a programming error. Selector `0xc` needs no redriver programming.
P0/P1 selector `0x8` instead uses the vendor three-chip SATA/xGbE profile
at `0x58`–`0x5a`, including its extra channel-register writes. Those
registers (`0x0d`, `0x14`, …) are undocumented in SNLS466A; their full bytes
are preserved without guessing RXDET semantics from adjacent registers.
Selector `0x9` does not run a redriver sequence.

**Ethernet limitation:** RXM100CB's eight Milan xGbE enable PCDs
(`0x148`–`0x14f`) default to false. Native policy preserves that gate and
warns if a mixed card requests Ethernet: the SATA/PCIe portions are still
configured, but Ethernet descriptors are omitted before DXIO. The shared
encoder now models Ethernet SerDes as type `0x10`, not PCIe, with separate
lane validation. This is not MAC/MDIO/PHY-link initialization: an enabled
Milan MAC configuration path has not been recovered or hardware-tested.
No factory MAC addresses are read or fabricated while these ports are off.
Mixed card routing and active SATA/redriver links are host-tested only;
the current bench straps `0x0000` do not exercise these paths.

Native hotplug retains empty bridges and advertises PCIe slots without
inventing physical slot numbers or external GPIO routing. The recovered
NVMe descriptors set LinkHotplug to 5 but leave the independent firmware
Hotpluggable header flag clear; the engine model preserves that distinction.
Only explicitly wired server-I2C hotplug entries generate SMU signal tables. Host tests cover
selector decoding, reduced-I/O policy, I2C preservation/error cleanup and
native-slot advertisement. Controller registration is verified, not physical
hot-insert/remove operation or signal integrity.

The console follows RXM100CB `AmdIdsDebugPrintPei`, GUID
`239e812c-ad4a-4d5a-8bb9-169131c344bf`, TE SHA-256 `8c36a0cc5a598aa6…`:
its default path uses FCH UART1 MMIO, software reset plus about 100 us
settle, divisor 26 and UART pad function 0 (`0xffd02c28`). The shared FCH
UART driver owns clock/power/pin/reset setup; the standard 16550 driver
sets framing and baud. Live FCH inventory supplies its ACPI description.
No assumption is made that Insyde enables the newer OpenBMC LPC VUART.
