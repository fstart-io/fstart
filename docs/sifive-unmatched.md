# SiFive HiFive Unmatched

The physical FU740 platform uses two stages:

- `bootblock` executes PRCI/UART/DDR initialization in LIM without allocating or installing a directory. It authenticates the pinned mainstage descriptor with the common bootstrap loader, then serializes the bounded media handoff.
- `main` runs in DRAM, with DRAM-backed writable state, heap and stack. It imports only the predecessor's bounded locator and authenticates its own firmware directory before payload dispatch.

L2 way zero remains cache. Usable LIM is `0x08000000..0x081e0000`, not the nominal 2 MiB aperture. LR/SC atomics are not supported in LIM; the allocator and directory runtime must run in cached DRAM. The mainstage is linked at `0x81000000`, below the OpenSBI/kernel/DTB reservations. Its fixed handoff is at `0x80000000`.

```sh
cargo fbuild assemble -b sifive-unmatched --release --payload halt
cargo fbuild assemble -b sifive-unmatched --release --payload linux \
  --input kernel=/path/to/Image-riscv64 \
  --input firmware=/path/to/fw_dynamic.bin \
  --input dtb=/path/to/unmatched.dtb
```

## Hardware validation

The 2026-10-31 halt test on the real 16-GiB board reached:

```text
fu740: 16384 MiB DRAM
handoff: 31 bytes at 0x80000000
mounting memory-mapped FFS at 0x20000000
fu740 ramstage: boot_integrity
fu740 ramstage: ready for payload
```

After 25 seconds the hart was stopped at `0x810005bc`, the mainstage halt payload's spin loop in the matching ELF; the trampoline's trap record remained zero.

The complete test-image prefix was programmed and read back exactly before execution. Only the assembler-patched initial-stage bytes were staged into LIM; the raw compiler binary does not have the authenticated bootstrap pin patched in. U74 hart 1 was entered through a `fence.i` trampoline while the other harts were stopped.

Two independent read-only collections of the uniquely addressable 16-MiB bank matched before writing. The original programmed prefix was restored and exactly readback-verified afterwards. The SD card and other disks were not written. Evidence is retained locally under `~/.local/share/fstart/recovery/unmatched-pr158-fix-lim-20261031T122644Z/` and the harness logs for `unmatched-authenticated-two-stage-halt-test`.

This is a **JTAG-started halt validation**, not a ROM cold-boot, Linux userspace, UEFI execution or SMP claim. The observed MSEL was `0xb` (SD). The plain FFS image here does not add a ROM-discovery GPT/FSBL header. The inherited Linux/UEFI peripheral and multi-hart bring-up is not validated by the halt result.

The onboard FT2232H's empty serial selects JTAG channel A (`--probe '0403:6010-0:'`); channel B is UART at 115200. Use the established board-specific bench procedures and back up before any flash write. Do not run RV64GC firmware on the S7 monitor hart, and do not confuse the 3-byte XIP bank with independently verified coverage of the entire physical flash chip.
