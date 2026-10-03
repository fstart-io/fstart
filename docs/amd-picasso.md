# Native AMD Picasso / Google Vilboz port

## Status: a foundation and cold-boot console checkpoint, NOT a complete port

This change is **not hardware-booted, not OS-bootable and not flash-ready**.
It has no FSP-M/FSP-S dependency or hidden binary call. It also does **not**
replace those binaries' work with no-ops and claim a successful native port.
There is deliberately no production `fbuild` metadata for this board yet.
The common build-plan/image executor needs an AMD packaging projection before
this can become a supported board. The checkpoint is built explicitly below.

## Ownership

- `crates/driver-amd`: register mechanisms, `tock-registers` MMIO, FCH banked
  GPIO/status/event routing, power/decode, Picasso pinmux/address/clock facts.
  No boards, FSP calls, payloads or stage loading. Real SoC availability is gated
  by the additive `picasso` feature.
- `crates/platform-amd`: fixed family flow, entry, DRAM layout, console adapter.
  Picasso is one SoC module, not a new crate per board or shared Intel CAR flow.
- `boards/google/zork`: shared Dalboz wiring and Vilboz CBI policy. Vilboz,
  Vilboz14 and Vilboz360 use the same wiring/flow and runtime EC facts. They are
  not three duplicated board packages. Other Zork variants are **not ported**.
- `crates/image-build/src/amd`: host-only EFS/PSP/BIOS wire format and APCB v2
  editor. Explicit little-endian `zerocopy` types; no native struct bitfield casts.
- `tools/amdfw`: thin Rust CLIs, `fstart-amdfw` and `fstart-apcb`. No runtime
  hardware policy in the command-line tool.

## Why x86 starts in DRAM

| Use | Physical range |
| --- | --- |
| PSP APOB | `02001000..02011000` |
| PSP transfer/shared buffer | `02011000..02019000` |
| Checkpoint BSS capacity | `02019000..02020000` |
| Identity page tables | `02020000..02026000` |
| Checkpoint stack (40 KiB) | `02026000..02030000` |
| **Immutable x86 reset image** | **`02030000..02050000`** |
| **x86 reset vector** | **`0204fff0`** |

The linker explicitly uses these **DRAM VMAs/LMAs**, not `ff000000` flash VMAs.
The PSP BIOS-directory entry type `62` copies an uncompressed 128-KiB image to
`02030000`; reset=1, copy=1, compressed=0. Reset enters the final 64-KiB segment,
loads a 32-bit-base GDT (the DRAM address exceeds 16 MiB), builds identity page
tables and enters 64-bit Rust on a DRAM stack. Cache remains disabled for this
checkpoint, so FCH accesses cannot accidentally be write-back cached.

The reset image is read-only throughout execution. Initialized writable `.data`
is deliberately rejected by a linker assertion. BSS, stack and page tables
are separate from the image, APOB and PSP work buffer. This preserves the
PSP reset-image integrity constraint; it does **not** implement S3 resume.
The linker reserves the complete early window for a future OS memory map.

## Source coverage and remaining work

**The opaque x86 FSP routines are a real completeness blocker.** Their parameter
structures name policies but do not reveal the silicon register sequences they
implement. Eliminating FSP requires documentation/reference implementations or
reverse engineering, followed by hardware validation. PSP-trained DRAM alone
is not proof that PCIe, graphics, security or power initialization is complete.

Specific board hazards that must survive the eventual native flow:

- Disable SPI ROM sharing before assigning GPIO67 to DMIC selection.
- Vilboz always has v3 WiFi wiring; failed CBI is **not** a pre-v3 fallback.
  Active-high WiFi power requires 10 ms off, then 50 ms on, then reset release.
  Revision >=2 uses active-low power and only the 50-ms delay.
- Source Dalboz PCIe GPIO40 is NVMe reset, but newer Vilboz uses it for a
  proximity sensor. Do not blindly execute that generic table before overriding
  it to input. This checkpoint deliberately does not execute that table.
- CBI revision failure falls back to old GPIO/codec/touchscreen policy; a valid
  zero-valued FW_CONFIG is not a cache-miss sentinel.
- The WWAN external-clock/PHY quirk starts **after** board revision 2, not at 2.
- APCB RAM-ID triples `[132,1,0], [90,1,0], [86,3,0], [69,1,0]` are **not** the
  same encoding as x86 GPIO IOMUX selectors. Do not substitute one for the other.

## Rust PSP tooling

Picasso has **one-level**, header-v0 directories, physical flash addresses and
no ISH or PSP A/B recovery. EFS points to PSP at offset `14` and BIOS at `1c`.
The initial directory starts on a **64-KiB** boundary, tables on 4 KiB and
ordinary blobs on 256 bytes. APCB/NVRAM allocations are erase aligned. Fletcher
covers header count/additional info and entries, excluding magic/checksum.
APOB NV uses Picasso's fixed `ff000000` quirk; PSP NV input is normalized from
an explicit flash offset to the actual physical mapping. Gen2 ROM Armor flags
must not accidentally appear on normal Gen1 PSP entries.

The output is a **partition fragment at the reported flash offset**, not a
factory chip image. The CLI refuses to overwrite existing output files.
It does not authenticate vendor blobs, sign ChromeOS PSP verstage, provide AMD
keys, synthesize APCB training parameters or make this checkpoint bootable on
a locked retail Chromebook. Do not flash it over a factory backup.

## Build and reproduce

From this workspace, build the checkpoint (no fbuild production integration):

```sh
cargo rustc --manifest-path boards/google/zork/Cargo.toml --release \
  --features checkpoint --bin picasso-checkpoint --target x86_64-unknown-none \
  -- -C relocation-model=static -C no-redzone=yes \
  -C link-arg=-T"$PWD/crates/platform-amd/linker/picasso.ld"
```

Extract only immutable image sections with binutils `objcopy -O binary`,
`--only-section` for `.text`, `.rodata`, `.init16`, `.reset`, `.last_byte`, and
`--gap-fill=0xff`. Verify the result is exactly `0x20000` bytes. `readelf -lW`
must show a DRAM LOAD at `02030000` and entry `0204fff0`.

