# AMD Onyx

AMD's SP5 reference board, one socket, EPYC 9004 (Genoa), twelve DDR5
channels, AST2600 BMC. The console is the SoC's UART1 (`0xfedca000`), the
BMC's SOL. SMM counts corrected errors (ten per thresholding SMI) and sends
each overflow to the BMC's SEL as a Platform Event Message on KCS3
(`0xCA2`), the BMC's one KCS channel, which the OS's IPMI driver shares:
only when it is idle; the boot does the same for the last boot's errors,
which it also puts in the BERT.

```sh
cargo fbuild build --board amd-onyx --release
# The image, packed from AMD's public Genoa blobs (coreboot's
# 3rdparty/amd_blobs, `genoa/PSP/...`) and the board's APCBs (`onyx/...`)
# in one directory:
cargo fbuild assemble --board amd-onyx --release --blobs /path/to/blobs
# Or extract them from a flash backup of the board:
cargo fbuild blobs --board amd-onyx --from flash-backup.bin --out /path/to/blobs
```

AMD publishes no firmware download for the Onyx, so its nightly bundle
and the web assembler take the donor from the user: a flash backup of the
board (docs/distribution.md).

## Flash image

fstart packs the 16 MiB the boot ROM maps: the EFS at `0x20000`, the
two-level PSP and BIOS directories with AMD's blobs and the board's APCBs,
the FFS at `[0x800000, 0xd00000)` and the reset image (BIOS directory
entry `0x62`) at `[0xd00000, 0x1000000)`, which the PSP copies to
`0x0a000000`. The PSP writes the APOB to `0x07001000`.

## Layout

| File | Content |
|---|---|
| `src/wiring.rs` | lane map from the public reference design, the MPIO requests, USB policy |
| `src/lib.rs` | the board's configuration of Genoa: NBIO, MPIO, USB, console, VGA |
