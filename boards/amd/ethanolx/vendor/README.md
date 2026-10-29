Put the board's own BIOS flash here as `RXM100CB.bin` (or pass
`--image-config /absolute/path`). It is vendor firmware and is not
distributed with fstart.

Either form works:

- the BMC's complete **64 MiB readout**; the build is then a full 64 MiB
  flash image plus the 16 MiB boot bank, or
- only its **lower 16 MiB** boot bank; the build is that bank.

The investigated readout and its lower 16 MiB have SHA-256:

```text
c0f6698446d5ab20dda9f2d95bf7e82e871c6e0b407c32262104eec982eae9ca  64 MiB readout
f8aed9803cb6fcee5ac9812e01d6923a89400e582de2a9260566932c48470f8a  lower 16 MiB
```

Milan EFS at `0x20000`; PSP directories `0x63000` / `0x483000`, BIOS
directories `0x2f3000` / `0x813000`. Vendor `0x62`: flash `0xcf0000`,
size `0x310000`, destination `0x75cf0000`. APOB destination `0x04000000`.
