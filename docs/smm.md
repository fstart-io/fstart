# fstart SMM image design

This document records the coreboot SMM model and the fstart SMM-image ABI.
The image is a **complete SMM image**: it contains the compiled Rust SMM
handler and all precompiled CPU entry points.  It is not a coreboot plugin
replacement and does not expect coreboot to provide or duplicate the permanent
SMM entry stub.  Coreboot integration is loader-side work: coreboot includes
the generated compatibility header, copies the image's entry/handler ranges to
SMRAM, patches data parameter blocks, and then uses its normal SMBASE
relocation flow.

## Coreboot reference behavior

Relevant coreboot files:

- `src/cpu/x86/smm/smm_module_loader.c` — parses SMM rmodules, computes the
  SMRAM layout, loads the permanent handler, installs page tables on x86_64,
  patches stub parameters, and copies staggered entry stubs.
- `src/cpu/x86/smm/smm_stub.S` — architectural SMI entry at
  `SMBASE + 0x8000`; switches from SMM real mode to protected/long mode,
  selects a stack from the APIC-id-to-CPU table, and calls the C/Rust handler.
- `src/include/cpu/x86/smm.h` — `smm_runtime`, `smm_module_params`, and
  `smm_stub_params` ABI definitions.
- `src/cpu/x86/smm/smm_module_handler.c` — permanent handler entry and
  dispatch (`cpu_smi_handler`, `northbridge_smi_handler`,
  `southbridge_smi_handler`).
- `src/mainboard/emulation/qemu-q35/{cpu.c,memmap.c,smihandler.c}` — Q35 SMRAM
  open/close/lock, save-state SMBASE relocation, global SMI enable, and QEMU
  save-state quirks.

Coreboot has two separate relocation concepts:

1. **rmodule relocation**: the SMM handler/stub are linked as relocatable
   modules and fixed up at runtime before being copied into SMRAM.
2. **SMBASE relocation**: every CPU initially enters SMM at the default SMBASE;
   the relocation handler edits that CPU's save state so future SMIs enter the
   permanent per-CPU SMBASE window in TSEG/SMRAM.

fstart keeps (2), because it is required by x86 hardware, but eliminates (1):
normal handler/stub code is position-independent and is never patched as code.

## fstart differences

- The number of entry points is known before building the SMM image:
  - coreboot compatibility builds use `CONFIG_MAX_CPUS`.
  - fstart boards declare it in `board.smm.entry_points`, defaulting to the
    `MpInit.num_cpus` value of the SMM-enabled stage.
- The SMM image contains **multiple precompiled PIC entry stubs**, one per
  configured CPU slot.  The installer copies entry `N` to CPU `N`'s
  `SMBASE + 0x8000`; it does not copy one canonical stub to all offsets.
- A native fstart header describes all image-relative offsets.  Optional
  coreboot compatibility emits a `.h` file with the same relative offsets so a
  coreboot loader can copy the right ranges without treating the image as an
  rmodule.
- The coreboot module-argument block is optional and feature-gated.  When
  enabled, its image-relative offset is also emitted in the compatibility
  header.

## Board RON schema

`BoardConfig` has an optional top-level `smm` block:

```ron
smm: Some((
    platform: QemuQ35,          // or PineviewIch7
    entry_points: Some(4),      // default: MpInit.num_cpus
    stack_size: 0x400,
    coreboot: (
        emit_header: true,
        module_args: true,
    ),
)),
```

An x86 stage enables SMM by declaring:

```ron
MpInit(cpu_model: "qemu-x86", num_cpus: 4, smm: true)
```

For the first implementation this capability belongs in a DRAM-backed stage,
after `MemoryInit` or `DramInit`.  CAR/XIP bootblocks should not install SMM.

## Native image format

`fstart-smm-image/build.rs` assembles the architectural SMM entry stub from
`crates/fstart-smm-image/asm/` and compiles the no_std Rust handler under
`crates/fstart-smm-image/handler/` into flat binary blobs, then emits the symbol
offsets consumed by `lib.rs`.  The image layout code therefore deals only in
prebuilt entry/handler bytes; it does not hand-construct instruction streams.

The image starts with `SmmImageHeader` from `fstart-smm::header`:

```text
u32 magic              "FSM1"
u16 version            currently 1
u16 header_size
u32 flags              bit 0 = coreboot module args present
u32 image_size
u16 entry_count
u16 entry_desc_size
u32 entries_offset     image-relative EntryDescriptor table
u32 common_offset      image-relative SMM handler/data blob
u32 common_size
u32 common_entry_offset offset inside copied handler blob to handler entry
u32 runtime_offset      offset inside copied handler blob to loader-filled runtime
u32 module_args_offset  0 when disabled
u32 module_args_size    0 when disabled
u32 stack_size
```

Each `EntryDescriptor` is also image-relative:

```text
u32 stub_offset
u32 stub_size
u32 entry_offset       usually 0; offset inside copied stub
u32 params_offset      offset inside copied stub to SmmEntryParams, or 0
```

All offsets are relative to byte 0 of the SMM image.  The copied code is PIC:
loaders may copy bytes and write data blocks (`SmmEntryParams`, runtime data,
coreboot module arguments), but must not apply relocation records to code.

Each entry stub may expose a `SmmEntryParams` block at `params_offset`:

```text
u32 cpu
u32 stack_size
u64 stack_top
u64 common_entry
u64 runtime
u64 coreboot_module_args
u64 cr3
u64 entry_base        absolute address where this entry stub was copied
u32 platform_kind     SMM dispatch backend selector
u32 platform_flags    dispatch-backend flags
u64 platform_data[4]  opaque dispatch-backend data
```

The entry stub is intentionally only an architectural trampoline.  It enters
long mode, switches to the per-CPU stack, calls the copied Rust SMM handler, and
executes `rsm` after the handler returns.  Platform-specific SMI source decode,
status clearing, ACPI enable/disable, and EOS handling live in Rust dispatch
modules selected by `platform_kind` and `platform_data`; they do not belong in
the entry assembly.

The runtime block also contains handler-maintained state after the per-CPU
save-state table: flags, a global SMI handler lock, the last APMC command,
per-command dispatch counters, and per-CPU SMI entry counters.  The Rust
handler uses these to serialize chipset dispatch/EOS like coreboot's handler
semaphore, make APMC finalize requests visible, and prove that every logical
CPU reached the permanent SMM handler.

Because entries are precompiled per slot, the permanent SMM path does not need
coreboot's APIC-ID-to-CPU lookup table.  The CPU slot is either baked into the
stub or written as data through `SmmEntryParams.cpu`.

## SMRAM layout

The hardware entry point remains `SMBASE + 0x8000`; save state lives at the top
of the 64 KiB SMBASE window and grows downward.  `fstart-smm::layout` mirrors
coreboot's placement checks:

```text
SMRAM top
+---------------------------+
| optional MSEG/board data  |
| Rust SMM handler/data     |
| optional page tables      |
+---------------------------+  first CPU segment base
| CPU 0 PIC entry stub      |  SMBASE + 0x8000
| CPU 0 save state          |  SMBASE + 0x10000 - save_state_size
+---------------------------+
| CPU 1/2/... staggered entries, avoiding save-state overlap
+---------------------------+
| per-CPU stacks            |
+---------------------------+  SMRAM base
```

`compute_cpu_layout()` returns the exact SMBASE, entry copy address,
save-state range, and stack range for each CPU slot.

## Coreboot compatibility header

When the SMM image crate is built with the `coreboot` feature and header output
enabled, its build script writes a header similar to:

```c
#pragma once
#define FSTART_SMM_NATIVE_HEADER_OFFSET 0u
#define FSTART_SMM_ENTRY_COUNT 4u
#define FSTART_SMM_ENTRY_DESC_SIZE 16u
#define FSTART_SMM_ENTRIES_OFFSET 44u
#define FSTART_SMM_COMMON_OFFSET 108u
#define FSTART_SMM_COMMON_ENTRY_OFFSET 0u
#define FSTART_SMM_RUNTIME_OFFSET 256u
#define FSTART_SMM_MODULE_ARGS_OFFSET 0u /* 0 when disabled */
```

The header deliberately exposes relative offsets, not absolute link addresses,
so coreboot can place the image wherever its SMRAM loader chooses.  Coreboot
must not build or copy its own permanent `smm_stub.S` for this path; it only
uses the generated offsets to locate and copy the entry stubs that are already
inside the SMM image.  Coreboot may still use its existing temporary default
SMRAM relocation handler unless/until the image grows a dedicated relocation
entry set.

## Platform hooks

The SMM entry assembly is not a platform hook point; it remains a tiny
architectural trampoline.  Platform-specific behavior is implemented by Rust
handler modules selected through `SmmEntryParams::platform_kind` and
`platform_data`.  The current `IntelIch` backend consumes PMBASE and GPE0_STS
offset values from `platform_data`; a future AMD backend can consume MMIO bases,
SMM MSR policy, and AMD save-state metadata without changing the entry stub.

A platform adapter is responsible for:

1. Discovering permanent SMRAM/TSEG base and size.
2. Opening SMRAM for writes.
3. Copying SMM handler code/data and the per-CPU precompiled PIC entry stubs using
   the native header or generated coreboot offsets and `compute_cpu_layout()`.
4. Patching data parameter blocks (`SmmEntryParams`, runtime, optional coreboot
   module args); never relocating code.
5. Installing the temporary default-SMRAM relocation handler.
6. Triggering a self-SMI on each CPU so the relocation handler writes that
   CPU's future SMBASE into save state.
7. Closing and locking SMRAM and enabling global SMI.

Q35 follows coreboot's `qemu-q35` sequence: open SMRAM, clear southbridge SMI
state, relocate using QEMU's AMD64/legacy save-state revision, close, enable
SMI, lock.  Pineview+ICH7 follows the same MP/SMM sequence with the ICH7 PMBASE,
TCO/APMC, and SMRAM controls exposed by the chipset drivers.  The x86 MP flight
plan now performs a default-SMRAM relocation step, a BSP-only post/lock step,
and a second all-CPU SMI step through the permanent handler before APs park, so
multi-core SMM entry is validated during firmware bring-up.

## Validation targets

- Coreboot QEMU q35:
  - enable Q35 SMM (`SMM_TSEG` or `SMM_ASEG`) and debug SMI logging
    (`DEBUG_SMI`, optionally runtime SMM loglevel) in Kconfig.
  - build under `~/src/coreboot` using `nix-shell` for required tools.
  - boot with QEMU q35 and trigger software SMI through APMC; verify SMM logs
    and stable repeated SMI handling.
- fstart QEMU q35:
  - add `board.smm.platform: QemuQ35` and `MpInit(..., smm: true)`.
  - run with matching `-smp` count.
  - verify one relocation per configured entry and repeated APMC SMI dispatch.
- fstart Pineview+ICH7:
  - add `board.smm.platform: PineviewIch7` and run SMM after DRAM/MP init,
    before final southbridge lockdown.
  - verify SMRAM closes/locks and software SMI reaches the permanent handler.
