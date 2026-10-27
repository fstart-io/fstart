# Vendor boot bank

Supply the **lower 16 MiB** of the board's own verified flash backup as
`RXM100CB.bin`, or use `--image-config /absolute/path/to/that-slice.bin`.
Do not commit firmware or NVRAM.

The investigated 64 MiB backup has SHA-256:

```text
c0f6698446d5ab20dda9f2d95bf7e82e871c6e0b407c32262104eec982eae9ca
```

Its lower 16 MiB slice has SHA-256:

```text
f8aed9803cb6fcee5ac9812e01d6923a89400e582de2a9260566932c48470f8a
```

Milan EFS at `0x20000`; PSP directories `0x63000` / `0x483000`, BIOS
directories `0x2f3000` / `0x813000`. Vendor `0x62`: flash `0xcf0000`,
size `0x310000`, destination `0x75cf0000`. APOB destination `0x04000000`.
The other vendor image at readout offset `0x03000000` must be preserved.
The BMC export size is not a confirmed physical flash-chip capacity.
