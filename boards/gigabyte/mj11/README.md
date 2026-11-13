# Gigabyte MJ11

MJ11-EC1 (G431-MM0) and MJ11-EC0 use the same host stage, with their
variant-specific board tables. Both have an AST2500 BMC.

## BMC console

The board waits for the BMC, enumerates PCI, then initializes its AST2500
natively at 1024×768, 60 Hz, XRGB8888. It publishes the framebuffer to the
payload without executing a VGA ROM. The shared DRAM must already be
initialized by the BMC; the host driver does not reset the BMC or train
its memory. The serial console remains available.

Use `uefi-ui` for CrabEFI's graphical boot manager with USB KVM keyboard
and mouse input, or `uefi` for its text menu:

```sh
cargo fbuild build --board gigabyte-mj11-ec1 --release --payload uefi-ui
cargo fbuild build --board gigabyte-mj11-ec0 --release --payload uefi-ui
```

For a full flash image, use `assemble` with `--blobs /path/to/blobs`.
The platform's firmware blob table and the board's APCB metadata describe
the required files. Supply firmware blobs matching the board revision.
