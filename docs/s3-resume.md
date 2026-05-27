# ACPI S3 resume implementation notes

This document records the fstart S3 resume model based on the coreboot paths for
Pineview/ICH7 and GM965/ICH8.

## Coreboot references

- `src/northbridge/intel/pineview/romstage.c`: detect S3, run resume DRAM init,
  recover CBMEM, set romstage handoff.
- `src/northbridge/intel/gm965/raminit.c`: load MRC cache before raminit,
  validate CPU/SPD-specific data, and stash updated training data after raminit.
- `src/drivers/mrc_cache/mrc_cache.c`: generic `MRCD` metadata, FMAP regions
  `RW_MRC_CACHE`, `RW_VAR_MRC_CACHE`, `RECOVERY_MRC_CACHE`, and
  `UNIFIED_MRC_CACHE`.
- `src/lib/prog_loaders.c`: use stage cache on S3, populate it on normal boot.
- `src/acpi/acpi.c` and `src/arch/x86/acpi_s3.c`: find FACS wake vector from
  old OS tables, copy trampoline to low memory, and jump without regenerating
  ACPI/SMBIOS.

## fstart pieces now in tree

- `fstart_types::BootPath`, `ResumeHandoff`, and `StageCacheInfo` model the
  early resume handoff.
- `fstart_services::resume` provides a global boot-path state and
  `ResumeDetector` trait.
- ICH7 and ICH8 implement `ResumeDetector` by checking PM1_CNT `SLP_TYP == S3`
  and publishing `BootPath::S3Resume`.
- `MemoryController::dram_init_with_boot_path()` lets northbridges select cold,
  warm, or S3-safe DRAM init. Pineview now maps S3 to the existing
  `BOOT_PATH_RESUME` value passed into native raminit. GM965 receives the boot
  path and currently logs the pending MRC fast path before using existing native
  training.
- `fstart_capabilities::mrc_cache` provides the generic persistent record
  format and helpers for SPI/FFS-backed MRC training data.
- `fstart_acpi::resume` can discover the preserved OS S3 wake vector from old
  RSDP/RSDT/XSDT/FADT/FACS tables.

## Remaining integration work

1. Add a generated `ResumeDetect` capability or call `ResumeDetector` from the
   existing early-init plan before `DramInit`.
2. Add board RON schema for SPI MRC regions and SMRAM/TSEG stage-cache ranges.
3. Wire chipset SPI/flash backends to `mrc_cache::read_record()` and a future
   write/stash path.
4. Teach GM965 native raminit to serialize/deserialize its training `sysinfo`
   equivalent and validate CPU/SPD checksums like coreboot.
5. Add SMRAM/TSEG compressed-stage cache population on normal boot and use on
   S3 before loading ramstage from SPI.
6. Add the x86 low-memory wake trampoline/jump and branch before ACPI/SMBIOS
   generation on `BootPath::S3Resume`.
7. Ensure normal-boot E820 reserves SMRAM/TSEG, stage cache, firmware heap,
   ACPI/SMBIOS tables, zero page, and any persistent handoff areas.
