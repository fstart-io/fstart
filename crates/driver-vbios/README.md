# x86 VGA option-ROM driver

BSP-only VGA option-ROM POST and VBE linear-framebuffer selection. The Rust
front end uses the existing PCI allocator's BAR windows, core MMIO/PIO helpers,
and timer service. It returns the common `FramebufferInfo` handoff consumed
by payloads; it does not implement a second PCI allocator or Radeon register
programming stack.

`vendor/` is the permissively licensed x86emu from coreboot's
`src/device/oprom/x86emu` and `src/device/oprom/include/x86emu`, taken from the
Picasso reference tree at `26618924835eea50107f1444e0a705993dc97785`. Original copyright/license notices and
`vendor/x86emu/LICENSE` are retained. The only emulator-core modification is
an execution-budget hook in `decode.c`. `compat/` supplies its coreboot build
interfaces; `shim.c` keeps packed emulator registers on the C side of the ABI.
Clang is required for the freestanding C build (provided by the Nix shell).

The ROM must be an authenticated board asset. The emulator has private guest
RAM rather than writing the platform's IVT/BDA/low memory. Hardware memory
accesses are restricted to the supplied GPU BAR/UMA windows and the legacy
VGA aperture; port I/O and PCI BIOS services require exclusive BSP ownership.
Each call has an instruction limit and a monotonic deadline. Unsupported
interrupts, invalid memory accesses, abnormal execution exits and invalid VBE
modes are errors, not successful display initialization. No retry is safe after
a partially executed ROM.

VBE modes are checked against the allocated framebuffer ranges, pitch, pixel
format and color masks. EDID's preferred timing is used when available; the
fallback preference is 1024x768. A framebuffer is returned only after a
successful mode set and a fresh mode-information query.
