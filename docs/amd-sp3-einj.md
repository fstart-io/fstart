# Native SP3 EINJ

Linux's standard ACPI EINJ interface drives a native SMM handler and the
retained PSP firmware. No DXE, UEFI protocol, synthetic `INT 18`, or MCA
status-register fabrication is involved.

## Deliberately restricted interface

- **Single-socket Rome only**, and only when live DF/UMC decoding and ECC
  checks succeed and PSP enable returns success. Other platforms emit no
  EINJ table. Naples, Milan and 2P support is not inferred from Rome.
- Only `0x8` (Memory Correctable) is advertised. No processor, PCIe,
  uncorrectable, deferred, fatal or vendor-defined error is accepted.
- Target addresses must lie inside the dedicated reserved scratch page
  `0x75bf1000..0x75bf2000`. Masks may select at most that page. The target
  is rounded down to a cache line. This is not an arbitrary-address service.
- The preceding page at `0x75bf0000` contains fixed ACPI parameters,
  backend status and the trigger table. Both pages are inside the existing
  reserved early region and outside the stage heap. There are no input
  pointers, nested buffers, OS-supplied mailbox addresses, or SMRAM exports.
- The shared request is copied once and validated. Authorization, operation
  state, saved NBIO registers and the four-attempt boot limit live in SMRAM.
- Each mailbox wait has a 20 ms polling bound. The first PSP failure
  permanently disables this service until reset; a timed-out command's
  SMRAM page is never reused by EINJ. These are polling-delay bounds, not
  hard wall-clock latency guarantees.

This restricted bring-up interface is not a general production EINJ
implementation. In particular, a corrected memory event does **not** test
PFEH #MC redirection, the native observer's journal, #MC-on-exit or BMC SEL.

## Components and sequence

- `driver-zen/src/psp/ras.rs`: command 22h wire types and live Rome target
  decoding. The accepted map subset is zero-based rule 0 below 4 GiB,
  no die/socket interleave, power-of-two or COD2/4/8 channel interleave,
  primary chip-select ranges. Unsupported/high-offset/ambiguous maps fail
  closed. Chip-select hashing and enabled/ECC checks use live registers.
- `platform-sp3/src/einj.rs`: fixed communication layout and input validation.
- `platform-sp3/src/acpi/einj.rs`: nine ACPI serialization actions and one
  memory-read trigger. The table appears only after SMM backend enable.
- `platform-sp3/src/smm/einj.rs`: native operation dispatcher. It uses the
  registered SMRAM C2P page and SMM flag, not an OS buffer or SMM stack.
- The S8030 SMM policy enables this restricted interface, not a new board
  variant. The installer config stays 32 bytes: the already-below-4-GiB PSP
  BAR uses a u32, making room for a prebound DF function address. The latter
  avoids `pci_types::PciAddress::new`'s panic path inside SMM while continuing
  to use the existing PCI registers, DF bitfields and SMN access helpers.

At SmmInfo registration, send PSP action **1** (enable). ACPI BEGIN sends
**2**. EXECUTE validates the request and the current target, saves NBIO SMN
`1d840/1d848/1d84c`, then sends **4** with physical address, UMC register
base, kind **1**, chip-select and socket. Linux then reads the selected
scratch line through the trigger table. END sends **3**, disables the NBIO
address-match control, restores low/high address and restores control, in
vendor order. Invalid target input closes the PSP operation itself because
Linux returns on INVALID without invoking ACPI END.

The PSP setup payload is packed: header `{size=28,status}`, action u32 at
+8, address u64 at +12, UMC base u32 at +20, kind u16 at +24, chip-select
u8 at +26, socket u8 at +27. Control messages have size 12. Header status
is checked; unlike the vendor callers, a positive nonzero PSP status is
not treated as success merely because it lacks EFI's high error bit.

## Evidence

Donor: **rome-s8030-v404**, S8030 V4.04; donor ROM hashes are pinned in the
adjacent analysis repository's `firmware/donors/SHA256SUMS`. VAs below are
module-relative, ImageBase 0. All code executes in SMM except the ACPI
builder and memory-configuration capture (DXE).

| Module / GUID | Executable SHA-256 | VAs / evidence |
|---|---|---|
| AmdPlatformRasSspSmm / `f8356c42-4ba6-706f-59e9-a9b1aff2bcd8` | `cb5c20f9dd3cebbfc8d5ea841c0c46345fd118210995368943b4b11681cf39df` | 6e90 buffer/action/flag; 6cd4 command 22h; 1d54 exact memory payload and preconditions; 1ee0 action 2; 1f38 action 3 and NBIO restoration; instruction-checked |
| AmdRasSspSmm / `ee8175ef-4c60-82af-f5e2-8a9ef1fc0a9a` | `cca68f7070ef3210e79a7a85c8a770d566253d0a4629aa2ec568ce56bc14a324` | protocol +8 → 628; 3a90 target selection, 4180 normalization, 3300 chip-select/hash; decompiler-derived, restricted subset |
| AmdRasSspDxe / `f71aab75-41be-c209-a324-9b963bbdcb26` | `8604a37b4af69061f904c326fa345cc1247108bb1068617c961d8319edfa5594` | 4bc live UMC register offsets underlying cached DIMM decode; decompiler-derived |
| AmdPlatformRasSspDxe / `820f3f0d-4d08-6c35-858a-d98ef42c051d` | `3dd7d3a6133e2f5e5afb8f0d11d78e80e8f5132c5ee898ec2abb903bed612641` | 6244 EINJ builder and runtime parameters; decompiler plus binary table layout |

The vendor default does **not** enable PSP injection: CBS
`AmdSetupSSP[0xb4]` is 0, copied to PCD 177h. The native policy deliberately
opts in and verifies command completion. Vendor support/configuration is
not itself evidence of successful hardware injection.

A second, independent gate is **UMC::MiscCfg.DisErrInj**, bit 2 at
UMC+1e0. CBS `AmdSetupSSP[0x120]` defaults to 1 (disable injection).
`CbsSetupDxeSSP`, GUID `8e38a88a-c267-4131-a8cd-c0bc80a24cb5`, executable
SHA-256 `941eb246eb7d3e7ae476369d8a27a77729bc847c317cb1c86c58b52e40a4859e`,
DXE VA **7722–7734**, instruction-checked, maps that byte to Rome APCB
byte token **ff756025**. The native service never clears this live gate.
The platform must be booted with injection permitted by its memory-init
configuration before EINJ can be published. Register names/offsets also
appear as facts in OpenSIL `cd8edaee7d42bc93dd4110981d15d96684271205`,
`xUSL/RAS/Common/Ras.h` (MiscCfg/EccCtrl).

ACPI format/semantics: ACPI 6.5 §18.6; usage and error types:
<https://docs.kernel.org/firmware-guide/acpi/apei/einj.html>.
Code is independently expressed from donor facts; no reference source
implementation was copied.

## Verification

Host checks cover exact wire layout, SMM-flagged mailbox sequencing and
PSP error propagation, channel/chip-select selection, unsupported maps,
scratch-only/type/mask validation, and ACPI checksums/action/trigger layout.
**271 tests passed** across driver-zen, platform-sp3 (including ACPI), SMM
and IPMI, with all three generation features enabled; two doc tests ignored.
Both normal S8030 and the existing PCIe wiring variant pass the real SMM
PIC, relocation and panic audits. LSP checks were inconclusive, not a
substitute for these builds.

`tools/hwtest/sp3-einj.c` is a disposable-initramfs test. Enable
`CONFIG_DEBUG_FS`, `CONFIG_ACPI_APEI`, `CONFIG_ACPI_APEI_EINJ`, `CONFIG_DEVMEM`
and the AMD64 EDAC driver. Set MCA polling `check_interval` to 1 second.
It mounts debugfs, requires exactly error type 8, records an EDAC baseline,
uses standard Linux EINJ to inject once in scratch, waits for a corrected
error count increase, and checks rejection of a non-scratch reserved page.
The read-only `/dev/mem` mapping is confined to the known communication page
for backend status; it does not read or write arbitrary host memory.

## Rome bench runs — 2026-10-26

Authorized S8030GM2NE / EPYC 7502P, 64 threads, 128 GiB. The disposable
Linux 7.1 image has no block filesystems or MD. Every flash operation writes
only the lower 16 MiB and verifies readback. A cooperative bench lease was
held throughout; a fresh two-read-identical pre-test lower-half backup was
retained before any write.

First boot (`einj-native1-20261026`): target/backend initialization did not
pass, so no EINJ table was published and **no injection occurred**. Linux
booted with EDAC CE=0/UE=0 and powered off. The helper correctly failed, with
all PSP injection-action status words untouched. A bounded output-only trace
was added to distinguish registration failure from live-map rejection;
validation was not weakened to make the test run.

Second boot (`einj-native2-20261026`): SmmInfo success, policy enabled,
channel 3 selected by the live COD8/hash map; `UMC3+1e0=0x117`, DisErrInj
set, so no injection attempted. Third boot (`einj-native3-20261026`) corrected
initialization order to match the vendor: enable PSP first, then validate the
UMC gate. Action 1 succeeded, but DisErrInj remained set. Again no EINJ table
and no injection. PSP enable alone does not override memory-init policy.

A disposable **composed test-image copy**, not a donor ROM, was then changed
with unmodified MPL-2.0 `amd-apcb` 0.5.5, commit
`1ec66d607de2437813cdd9ffa91719a4b163b3cf`: only token ff756025 from 1 to 0
in the two existing APCBs at 4ce000 and 4d0000. The crate's normal `save()` updates the unique-instance counters so vendor
memory init can recognize the configuration change. The changed bytes are
4ce1fc/4d021c (values) and 4ce00c/4d000c (counter increments). The token decrement
and counter increment cancel in the byte-sum checksum; both APCB checksums
were independently verified and sizes are unchanged. An initial checksum-only
copy was flashed but not booted, then superseded by this counter-updated copy.
The upper flash half and all other bytes are identical. No live gate register
was forced, and data scrambling was not disabled.

Fourth boot (`einj-native4-20261026`) **passed the real correctable-memory
injection test**:

- DisErrInj changed through vendor memory init: UMC3 MiscCfg `117 → 113`;
  EccCtrl `673` still has read checking and ECC generation enabled.
- Linux initialized the native EINJ table and advertised only type 8.
- PSP enable/begin/setup/end all completed with status 0.
- One standard Linux EINJ request targeted reserved scratch `75bf1000`.
- A genuine corrected UMC event was logged: CPU 2, bank 18, status
  `942040000000011b`, IPID `0000009600350f00`, syndrome `7c7600010a800100`.
  EDAC CE count increased **0 → 1**; no synthetic `INT 18` was used.
- Hardware normalized address `0eb7e000` and UMC3 match the live decoder's
  selected physical target. The tiny kernel lacks `CONFIG_AMD_ATL`, so EDAC
  printed **Cannot decode normalized address** instead of reconstructing the
  system address/DIMM location. This is not proof of complete DIMM attribution.
- A subsequent request for the non-scratch communication page was rejected
  with `EINVAL`; backend command status was 2 and END completed successfully.
- Linux powered off normally. No panic/Oops/BUG or host disk writes.

This validates ACPI → native SMM → PSP → real corrected DRAM error → native
Linux MCA/EDAC handling. It does **not** validate hardware #MC redirection,
#MC-on-exit, the SMM MCA journal or BMC logging. The old bounded KCS completion
failure documented in `amd-sp3-ras.md` is independent and remains unresolved.

Fifth boot (`einj-native5-20261026`) repeated the same test with the final
source, including typed UMC register fields. It also passed: CE **0 → 1**,
the same UMC3 normalized address/status/IPID/syndrome, non-scratch `EINVAL`,
all PSP actions status 0, normal ACPI poweroff. Thus real corrected-error
injection was observed on **two independent boots**, not just command success.

Final tested artifacts (SHA-256):

- SMM: `caf872c7584a0d9cfd1b44bb52f0e9a62820c8d3044e6a61e8c1fa7b92a20dd6`.
- Composed test-image lower half: `279634dfcce9dc06ebfc79b2f7090bf9d0f0124960b09daa3373becb7e871123`.
- Complete test image: `0fe52837ba9e43d393f4ca454cdc246fba8c2c669b71e6f6a1b6dd39e222bcd5`.
- Test kernel/initramfs: `6b5a7894b4f96438502aff47abad13e185e900bbc6813d842823ea62faf7c91e`.

Logs remain local in the analysis repository:
`firmware/hw/s8030gm2ne-CCSF2AK45016/fstart-boot-einj-native{1,2,3,4,5}-20261026.log`.

The **exact pre-test firmware was restored** after the final run. Flashrom
verified it and an independent lower-half readback matched SHA-256
`55ee3e777f6e7bbb47d18873c64ff7d0ab4a5657489b51b4e7adc9067ec2401c`.
The host is off, BMC SPI is unbound, SPI_SEL is back to host, its GPIO holder
was released, and the cooperative bench lease was released. The injection-
enabled APCB configuration was not left deployed. No host data disks were
written.
