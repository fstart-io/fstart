# Native AMD Zeppelin / Gigabyte MJ11

Native (no AGESA) port of AMD Zeppelin (EPYC Embedded 3000, Family 17h model
01h) and the Gigabyte MJ11-EC1/EC0, from the vendor firmware analysis in
`sp4r2`. It boots Linux, and
CrabEFI with an OS behind it, on an MJ11-EC1 (EPYC 3151, SP4r2, one die).

## Code layout

- `crates/driver-amd/src/fam17h`: what Zeppelin and Picasso share (SMN/DF
  transports, SMU mailbox, PSP mailbox, microcode, APOB, BIOS directory, DXIO
  descriptors, cache plan, MSRs).
- `crates/driver-amd/src/zeppelin`: the SoC (topology, DF, NBIO, DXIO, CCX,
  SMU, FCH incl. SATA/USB/multi-die). Gated by the `zeppelin` feature.
- `crates/platform-amd/src/zeppelin`: the boot flow (`stage.rs`), DRAM
  layout, CPU driver for the MP engine, SMM and PSP SMM info (`smm.rs`), PCI
  root, e820, ACPI tables, payload handoff and the host build plan.
- SMM: `fstart_arch::x86::cpu::amd::smm` (TSEG/SMBASE MSRs), the FCH SMI
  control and handler in `crates/driver-amd/src/fch/smi.rs`, the Zeppelin
  sleep entry in `zeppelin/fch/smm.rs`.
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
(`crates/image-build/src/amd/vendor_flash.rs`). The microcode comes from the
vendor directories at runtime (`$BHD` → `$BL2`, type 0x66).

## x86 starts in DRAM

The PSP trains DRAM, writes the APOB to `30000000` and releases the BSP at
the end of the copied reset image. `asm/psp_reset.S` (shared with Picasso)
sends every other CPU to the SIPI trampoline at `0x8000`, copies `.data`,
clears BSS and enters long mode. Layout (`zeppelin/layout.rs`):

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

- S3, APOB NV (MRC cache), multi-die and the EC0 variant on hardware.
- Board extras of the vendor firmware: the BMC ready wait and the PM1
  cleanup of AmdCpmOemInitPeim.
