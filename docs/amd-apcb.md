# AMD APCB settings at image build time

The ABL (the PSP's memory and fabric init, before x86 runs) reads its
board settings from the APCBs in the BIOS directory (entries `0x60` and
its backups `0x68`): tokens (a 32-bit ID and a value) and typed entries.
An AMD board whose APCBs come from its vendor's image (`fbuild blobs`,
`--donor`) inherits the vendor's choices. A board states the ones it
depends on in its `[package.metadata.fstart.amd]` table, and the image
build writes them into every APCB it packs. Both work on APCB v3
(`APCB` header version `0x30`), in two-level and ISH partitions; Picasso's
APCB v2 template (`apcb = { template = ... }`) takes neither.

```toml
[package.metadata.fstart.amd]
apcb-tokens = [
  { name = "DfPciMmioSize", value = 0x1000_0000 },
  { id = 0x1ccc9834, value = 1 },            # NbioIommu
  { id = 0x12345678, value = true, size = "bool" },
]
cxl-regions = [
  { size = 0x10_0000_0000, socket = 0, die = 1, nbio-map = 2, memory = "volatile" },
]
```

## Tokens (`apcb-tokens`)

- `id` or `name`: the token. Names are those of
  `fstart_amdfw_format::apcb::names` (generated from the decoded Venice
  donors, venice `reports/research/VALUES/apcb-tokens.json`): Oxide's
  amd-apcb names, and the CBS setup field names GIGABYTE syncs into the
  tokens as aliases (`CbsDfDbgNumPciSegments` is `DfPciMmioSize`). Both
  together must name the same token.
- `value`: an integer up to `u32`, or `true`/`false`.
- `size` (optional): the token entry kind, `1`, `2`, `4` or `"bool"`.
  Without it, the kind of the entries that already carry the token; a
  named token no APCB carries is a dword (both Venice donors keep every
  token in dword entries); an unnamed one needs a size.

What the build does (`fstart_image_build::amd::apcb_tokens`, on
`fstart_amdfw_format::apcb::token`):

- A token some APCB carries is rewritten in every APCB and every entry of
  its kind that carries it: all instances, board masks and priorities, so
  no copy the ABL could prefer keeps another value. It is not added to the
  APCBs that lack it.
- A token no APCB carries is added to every APCB with a token entry of its
  kind, in the entry the ABL ranks first for every board (board mask
  `0xFFFF`, then the highest priority: HardForce `0x01` ... Normal `0x20`),
  keeping the entry sorted by ID. The APCB grows by 8 bytes into erased
  room after it (and after its 0x100-byte HMAC tail, which moves along);
  an APCB file with no room left (MD25-CG0's `0x68/8`) grows.
- A fusing token (`apcb::names::IN_PLACE_ONLY`: `PspPsbAutoFuse`,
  `PspOpnPlatformComptFusing`) is only ever rewritten where an APCB
  carries it, never added: the ABL reads a token by ID and entry kind, and
  one added with another kind would be ignored, leaving the ABL's default,
  which may be to fuse. Naming one that no APCB carries fails the build.
- A token that lands in no APCB, a kind other than the APCBs carry it as,
  a value wider than its kind, or a token named twice fail the build, and
  so does an APCB that carries the token but does not read the board's
  value back afterwards.
- An APCB whose bytes changed is sealed: unique instance + 1 (the ABL's
  "settings changed"), the byte checksum, the HMAC tail erased to 0xFF
  ("no HMAC"; a stale one would not verify). One whose bytes did not change
  is packed as the board's file has it: a board that names its donor's
  values packs the donor's APCBs byte for byte.

A token the ABL reads and the firmware relies on belongs in the table even
when it has the donor's value: the build then fails instead of packing
another value if the donor's APCB changes, and a host test can check the
firmware's configuration against it. SP7 boards call
`fstart_platform_amd::sp7::apcb::disagreement` with their table's tokens
(the ECAM tokens against `sp7::Config::ecam`, `NbioIommu` against the NBIO
policy); see `boards/gigabyte/md25-cg0`'s `apcb_tokens_agree_with_the_config`.

## CXL regions (`cxl-regions`)

The DF group's CXL region array (type `0xCC`, 48 descriptors of 0x1C
bytes) of every APCB that has one: the regions the ABL maps and gives a
DRAM rule (`fstart_amdfw_format::apcb::CxlRegion`). The array is rewritten
whole (unused descriptors zeroed); the board's APCB must carry it. SP7's
runtime CXL update rewrites it in flash too (`sp7::cxl_apcb`).

## GIGABYTE MD25-CG0

Its table names the ABL tokens its code relies on, with GIGABYTE's F01
values but for `PspPsbAutoFuse`, so its APCBs are the donor's byte for
byte except the one that carries that token (`0x68/0`, sealed again):

| Token | Value | What relies on it |
|---|---|---|
| `DfPciMmioBaseLo/Hi`, `DfPciMmioSize` | `0x2FFC_8000_0000`, 256 MiB | `sp7::Config::ecam` (the vendor asserts the x86 ECAM equals them, 2P_COMPLETE §2.2) |
| `NbioIommu` | 1 | `nbio::wh::Policy::iommu` |
| `FchRom3BaseHigh` | `0x2FFD` | `sp7::flash` reads the reset image through ROM3 inside the 128 TiB identity map |
| `BmcInitBeforeDram` | 1 | the AST2700 link `Mpio::early_bmc` keeps (Dell has 0 with an early BMC link too) |
| `FchUsb0..3Enable` (`CbsCmnFchUsbXHCI0..3Enable`) | 1 | `Config::usb` is `None`: USB as the ABL set it up |
| `PspPsbAutoFuse` | **0** (F01: 1) | never fuse PSB: see below |

Not named, so the ABL's defaults or the donor's values apply
(VALUES.md §2.5): `CcxSmtControl`, `CcxApicMode`, `DfDramNumaPerSocket`
and `CcxCoreControl` are absent from GIGABYTE's APCBs (SMT on, x2APIC
auto, the default NPS; fstart reads the result from the APOB and CPUID);
`DfBottomIo` `0x80` (fstart reads TOM); `DfXgmiConfig` 3 (1P);
`CcxCpuWatchdogEnable` 1 (fstart sets the CPU watchdog itself).

`PspPsbAutoFuse` is the one token the MD25-CG0 sets against its donor:
GIGABYTE ships **1** (Dell 0), which lets the PSP fuse Platform Secure
Boot on first boot to the signing key of the image it booted. A fused
CPU then only runs images signed with that key, which an fstart image is
not, so the board sets it to **0**. Every Venice board should.

**This protects only a CPU that has never booted the vendor's image.** A
CPU that ever booted GIGABYTE's F01 (or another image with the token at
1) may already have fused PSB to GIGABYTE's key, irreversibly, and then
refuses to run an fstart image at all. Use a CPU never booted with F01,
or check its PSB fuse state (the PSP's PSB status, e.g. through the
vendor firmware's setup or a PSP tool) before flashing fstart.
The eSPI setup the board reads
(`EspiInit`) and the early PCIe link are APCB entries, not tokens.
