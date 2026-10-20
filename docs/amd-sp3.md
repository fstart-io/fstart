# AMD SP3: Naples, Rome, Milan

Native (no AGESA) support for the AMD SP3 socket in one and two socket
systems, and the Tyan S8030 board. Status: **in development, not booted**.

| Generation | Silicon | CPUID | Notes |
|---|---|---|---|
| Naples (EPYC 7001) | ZP, DF2 | Fam 17h M00h–0Fh | 4 dies per socket |
| Rome (EPYC 7002) | SSP, DF3 | Fam 17h M30h–3Fh | IOD + up to 8 CCDs |
| Milan (EPYC 7003) | GN, DF3 | Fam 19h M00h–0Fh | IOD + up to 8 CCDs |

## Code layout

- `crates/driver-zen`: silicon IP blocks (`smn`, `soc`, `topology`, `df`,
  `nbio`, `smu`, `dxio`, `ccx`, `fch`, `psp`, `apob`), one implementation
  per IP version, gated by the `naples`/`rome`/`milan` features.
- `crates/platform-sp3`: the per-generation boot flow, memory map, MP, SMM,
  PCI root bridges, ACPI and the host build plan / flash image.
- `boards/tyan/s8030`: variants `tyan-s8030-rome` (lower 16 MiB of the
  32 MiB flash) and `tyan-s8030-milan` (upper 16 MiB).

## Flash image

The PSP, ABL, SMU, PMU and DXIO firmware and the APCB stay vendor blobs.
Each 16 MiB half keeps the vendor prefix (EFS, PSP and BIOS directories)
and replaces the reset image (BIOS directory entry 0x62). The PSP boot ROM
picks the half by the EFS generation word: Rome must be the lower half,
Milan the upper.

The reverse engineering behind each block is in the `sp3` analysis
workspace (`reports/*_SP3.md`, `reports/NATIVE_PORT_PLAN.md`).
