# Native AMD Zeppelin / Gigabyte MJ11

Native (no AGESA) port of AMD Zeppelin (EPYC Embedded 3000, Family 17h model
01h) and the Gigabyte MJ11-EC1/EC0, from the vendor firmware analysis in
`sp4r2`. It boots Linux, and
CrabEFI with an OS behind it, on an MJ11-EC1 (EPYC 3151, SP4r2, one die).

## Code layout

- `crates/driver-amd`: the silicon, by IP block. A block has its common
  code and a submodule per IP version where the code differs, named by its
  AGESA version or, without one, by the SoC (as openSIL's `xUSL/<IP>`):
  - `smn`, `df`: the register transports. `df` also has the dies (`Die`,
    `dies`), the address map and its resource split (`df::map`) and the
    fabric init (`df::zeppelin`).
  - `smu`: the mailbox, the AP release (`smu::threads`) and Zeppelin's SMU
    V9 messages (`smu::v9`).
  - `nbio`: `iohc` (DRAM map, BARs), `ioapic`, `iommu`, `pcie`; the NBIO
    base init and tables (`nbio::zeppelin`).
  - `dxio`: the DXIO firmware client; the lane mapping and PCIe setup
    (`dxio::zeppelin`).
  - `ccx`: per-CPU init, MSRs, cache plan, microcode and TSC; the reset MSR
    table (`ccx::zeppelin`).
  - `psp` (mailbox), `apob`, `table` (vendor register
    tables).
  - `fch`: the register layer and controllers (LPC, SPI, eSPI, GPIO, I2C,
    SMI, ...); the Taishan FCH (`fch::taishan`: SATA, USB, IRQ routing, the
    multi-die FCH, the SMM parts).
  - `soc::zeppelin`: the SoC itself: its `Soc` constants, identity
    (package, revision), non-PCI pool, ECAM and APOB location.

  The Zeppelin versions are gated by the `zeppelin` feature.
- `crates/platform-amd/src/zeppelin`: Zeppelin on the shared noncar flow
  ([amd-noncar.md](amd-noncar.md)): the `noncar::Soc` hooks, the silicon
  init sequence (`silicon.rs`), the board `Config`, DRAM layout, ACPI data
  and its host policy. The flow, MP, SMM and PSP SMM info, PCI root,
  e820, ACPI tables and payload handoff are the shared ones in
  `crates/platform-amd/src/noncar` and `src/acpi.rs`.
- SMM: `fstart_arch::x86::cpu::amd::smm` (TSEG/SMBASE MSRs), the FCH SMI
  control and handler in `crates/driver-amd/src/fch/smi.rs`, the Zeppelin
  sleep entry in `fch/taishan/smm.rs`.
- `boards/gigabyte/mj11`: DXIO lanes (EC1 x8/x8, EC0 x16), interrupt routes,
  AST2500 SUART1 console, IPMI KCS and SPMI, and the vendor tables of each
  variant: VR register images, PCIe re-drivers (EC1) and post-training SMN
  values.
- `crates/driver-i2c`: the DesignWare I2C master of the FCH (I2C3 reaches
  the VRs and re-drivers).
- `crates/driver-superio/src/aspeed_ast2400.rs`: the AST2400/AST2500 SIO.

## Flash image

The image keeps the vendor PSP firmware: the first `0x294000` bytes of the
vendor flash (EFS, PSP and BIOS directories, APCB, ABL, SMU firmware,
microcode) stay byte-identical. fstart replaces BIOS directory entry 0x62:

| Flash | Use |
| --- | --- |
| `000000..294000` | vendor prefix |
| `294000..db0000` | FFS (payload, boot policy) |
| `db0000..1000000` | reset image (PSP copies it to `76db0000`) |

`fbuild assemble --image-config <vendor flash dump>` composes the image
(`crates/image-build/src/amd/assembly.rs`). The vendor image's BIOS
directory, the level 2 the PSP boots from (`EFS` → `$BHD` → `$BL2`), must
describe this reset image and APOB and lie within the prefix. Its microcode
entries (type 0x66) go into the FFS as `cpu_microcode.bin`.

## x86 starts in DRAM

The PSP trains DRAM, writes the APOB to `30000000` and releases the BSP at
the end of the copied reset image. `asm/psp_reset.S` (shared with Picasso)
sends every other CPU to the SIPI trampoline at `0x8000`, copies `.data`,
clears BSS and enters long mode. Layout (`zeppelin/layout.rs`, a
`noncar::layout::Layout`):

| Use | Range |
| --- | --- |
| Page tables | `76000000..76006000` |
| Data, BSS (ACPI tables, AP stacks) | `76010000..76200000` |
| Heap | `76200000..76a00000` |
| BSP stack | `76d00000..76db0000` |
| Reset image | `76db0000..77000000` |
| TSEG | 8 MiB, size aligned, at the top of the DRAM below 4 GiB |

`76000000..77000000`, the APOB, the AP bootstrap page and TSEG with
everything above it stay reserved in e820: parked APs and the ACPI tables live
there.

## Boot flow

Steps 1-4 are the SoC's TP1
(`silicon_init`), 5 the flow with TP2, 6 TP3 and the flow:

1. Console (LPC decode, AST2500 SUART1), FCH early init, DF MMIO/IO map,
   DF early/scrub init, FCH reset tasks, board VRs and re-drivers over I2C3,
   NBIO early init, PSP BAR, BSP CCX init, NBIO TOM.
2. NBIO IOAPIC and IOMMU MMIO, DXIO training per die, board post-training
   SMN values, FCH USB, SMU init, DF late init, NBIO pre-enumeration, FCH
   env/SATA/xGbE.
3. MP: the SMU releases the APs (ThreadEn) into the trampoline. They load
   microcode and run the CCX init. SYSCFG.MtrrFixDramModEn is set during the
   MTRR replay and cleared afterwards.
4. SMM: the board's handler goes into TSEG,
   every CPU gets its SMBASE and locks SMM, then the FCH routes the SMI
   command port (0xb2), SLP_TYP, the power button and the board's GEVENTs
   (MJ11: GEVENT11, the BMC's diagnostic interrupt button) to SMI. The PSP
   gets the SMM info (mailbox command 2) right away.
5. PCI enumeration, IOAPICs, ACPI (MADT, FADT, HPET, MCFG, IVRS, SPMI, DSDT
   with root bridges, `_PRT`s, SIO COM1, IPMI KCS). The internal bridges' own INTA
   goes to their NBIO IOAPIC pin, as in the vendor DSDT. SCI_EN stays clear:
   the OS enables ACPI with 0xe1 to the FADT's SMI_CMD.
6. Vendor ReadyToBoot steps, the DF extended config off, PSP LockDFReg, then
   PSP BootDone right before the payload.

The SMM handler does ACPI enable/disable, power button to S5 while the OS
hasn't enabled ACPI, and S3-S5 entry on SLP_TYP with the Zeppelin steps.
The sleep is entered as the vendor's power button path does (SlpTypeEn,
then SLP_EN), not with an SMU message. S0/S1 requests are dropped.

## Building

```sh
cargo fbuild assemble --board gigabyte-mj11-ec1 --release \
  --image-config <vendor flash dump> \
  --payload linux --kernel bzImage --linux-bootargs "console=ttyS0,115200"
# or --payload uefi for CrabEFI
```

`gigabyte-mj11-ec0` selects the x16 slot variant.

## Not done

- S3: the vendor DSDT has no `_S3` either, so the OS only sees S0 and S5.
- APOB NV (the vendor's copy of the APOB in BIOS directory entry 0x63):
  vendor x86 code reads it only on S3 resume and capsule boots. It lives in
  the vendor flash prefix, which stays byte-identical.
- Multi-die (SP4) and the EC0 variant are built but not tested on hardware.
