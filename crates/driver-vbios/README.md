# x86 VGA option-ROM driver

BSP-only VGA option-ROM POST and VBE linear-framebuffer selection, using the
native Rust [rsemu](https://github.com/KarpelesLab/rsemu) interpreter with
`no_std + alloc`. There is no C emulator, shim, build script or C fallback.

The driver uses the existing PCI allocator's BAR windows, core MMIO/PIO
helpers, and timer service. It returns the common `FramebufferInfo` handoff
consumed by payloads; it does not implement another PCI allocator or a Radeon
register-programming stack. The pinned rsemu fork adds bounded interpreter
steps and explicit exits.

The ROM must be an authenticated board asset. The interpreter has private
RAM rather than writing the platform's IVT/BDA/low memory. Hardware memory
accesses are restricted to the supplied GPU BAR/UMA windows and the legacy
VGA aperture; port I/O and PCI BIOS services require exclusive BSP ownership.
Each call has an instruction/step limit and a monotonic deadline. REP yields
within the interpreter, so a large count cannot bypass the deadline.
Unsupported interrupts, memory faults, abnormal execution exits and invalid
VBE modes are errors, not successful display initialization. No retry is safe
after a partially executed ROM.

VBE modes are checked against allocated framebuffer ranges, pitch, pixel
format and color masks. EDID's preferred timing is used when available; the
fallback preference is 1024x768. A framebuffer is returned only after a
successful mode set and a fresh mode-information query.
