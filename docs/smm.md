# fstart SMM image design

fstart builds a complete position-independent SMM image containing the Rust
handler and one precompiled architectural entry stub per configured CPU slot.
The installer copies it into SMRAM, writes the runtime block and handler
configuration, relocates every online CPU's SMBASE, and locks the chipset
window.

The format is deliberately **unversioned**. fstart does not maintain a stable
loader ABI yet; format changes update the builder and installer together.

## Coreboot reference

The flow follows coreboot's x86 SMM model:

- `src/cpu/x86/mp_init.c` — serialized self-SMI relocation through the
  default SMBASE (`smm_initiate_relocation`).
- `src/cpu/x86/smm/smm_module_loader.c` — staggered SMBASE windows, per-CPU
  stacks, and permanent page tables inside SMRAM.
- `src/cpu/x86/smm/smm_module_handler.c` — permanent handler rendezvous
  (`smi_obtain_lock`): one owner, the others wait inside SMM.
- `src/cpu/intel/smm/gen1/smmrelocate.c` — Intel save-state SMBASE write and
  SMRR programming in the relocation handler; alternative SMRR pair on
  model 0Fh/17h/1Ch CPUs.
- `src/cpu/intel/model_1067x/mp_init.c`, `src/cpu/intel/common/common_init.c`
  — `IA32_FEATURE_CONTROL` policy (VMX + SMRR enable + lock).
- `src/mainboard/emulation/qemu-q35/cpu.c` — AMD64 save-state relocation.

coreboot also relocates the handler as an rmodule; fstart does not. Handler
code is position independent and never patched.

## Boundaries

- `fstart-smm` owns the image header, checked SMRAM layout, runtime ABI,
  volatile save-state SMBASE access, and byte-copy installer.
- `fstart-image-build` links and audits the relocatable handler memory image.
- `fstart_arch::x86::cpu::intel::smm::IntelSmm` owns the post-MP lifecycle. It
  is composed from three owners and makes no chipset, board, or CPU-model
  decision itself:
  - the northbridge's `SmramControl` (TSEG geometry, open/close/lock);
  - the southbridge's `SmiControl` (SMI sources, handler configuration);
  - the CPU model driver's `SmmCpu` (save-state layout, SMRR register pair).
- CPU model drivers (`core2_cpu`, `pineview`) own `IA32_FEATURE_CONTROL`
  policy in `init_cpu`, as coreboot's model drivers do.
- `driver-intel` owns ICH SMI source decoding, acknowledgement, and the
  handler configuration type.
- Board SMM modules bind only their concrete handler.

SMM is ordinary post-MP platform work. `mp_init()` brings CPUs online and
returns `MpHandle`; it has no chipset callback or SMM flight-plan steps.

## Native FSMM format

`SmmImageHeader` starts with the `FSMM` magic and structural sizes, but no
version field. File offsets address serialized bytes; handler/runtime offsets
are relative to the copied handler memory base.

```text
u32 magic                    "FSMM"
u16 header_size
u16 entry_desc_size
u32 flags                    bit 0 coreboot module args, bit 1 coreboot header
u32 image_size
u16 entry_count
u16 reserved
u32 entries_offset           file-relative descriptor table
u32 handler_offset           file-relative initialized handler bytes
u32 handler_load_size        .text + .rodata + .data and alignment gaps
u32 handler_mem_size         complete extent including .bss/runtime/config
u32 handler_entry_offset     handler-memory-relative
u32 runtime_offset           handler-memory-relative
u32 runtime_size
u32 handler_config_offset    handler-memory-relative
u32 handler_config_capacity
u32 module_args_offset       handler-memory-relative, zero when absent
u32 module_args_size
u32 stack_size
```

Each fixed-size `EntryDescriptor` contains file-relative `stub_offset`,
`stub_size`, `entry_offset`, and `params_offset` fields.

The linker lays out one contiguous memory image at VMA zero:

```text
.text -> .rodata -> .data -> .bss (NOLOAD)
```

The builder preserves VMA alignment gaps in the initialized byte range. The
installer zeros the complete `handler_mem_size`, copies `handler_load_size`,
and then writes the runtime block, the handler configuration, and optional
module arguments. One load delta therefore applies to every
compiler-generated cross-section reference.

The final ELF retains relocation records for auditing. Allocated sections other
than `.text`, `.rodata`, `.data`, and `.bss` are rejected. GOT/PLT-indirect
calls, absolute/GOT/TLS/dynamic relocations, undefined symbols, and Rust panic
machinery are also rejected. A PC-relative relocation is accepted only when its
target resolves inside the copied image. No loader applies relocations to the
shipped bytes.

## Entry and runtime ABI

`SmmEntryParams` is loader-filled data inside each copied stub:

```text
u32 cpu
u32 stack_size
u64 stack_top
u64 common_entry
u64 runtime
u64 coreboot_module_args
u64 cr3
u64 entry_base
```

`SmmRuntime` holds the SMRAM range, the active CPU count, the stack size, the
rendezvous owner word, and the offset and size of the handler configuration.
It holds no pointers and no per-CPU tables.

The handler configuration is a plain Rust value. The concrete handler names
its type as `SmmHandler::Config`, and the matching southbridge names the same
type as `SmiControl::HandlerConfig`; for ICH that is
`driver_intel::southbridge::smi::IchSmmConfig` (PMBASE and GPE0 geometry).
The installer writes the value into a fixed 256-byte, 16-byte-aligned slot and
records its size. The SMM entry copies it out only if the recorded size is
exactly `size_of::<Config>()` and the slot lies inside SMRAM, then passes it to
the handler. There is no serialization format, magic, or schema version.

Every permanent entry follows coreboot's rendezvous: one CPU atomically claims
ownership and runs chipset dispatch, all other entrants remain in SMM until
the owner releases the lock, and waiters then return through RSM. The owner
word lives in TSEG; locked access to it relies on SMRR giving TSEG a
write-back type inside SMM, as on coreboot.

## Page tables and SMRAM layout

Permanent x86-64 entries never inherit ramstage CR3. The layout reserves six
pages in SMRAM for a PML4, PDPT, and four 2 MiB-page directories covering the
low 4 GiB. The installer builds those tables before lock and patches their CR3
into every permanent entry stub.

The temporary default-SMBASE relocation stub uses a separate identity-map set
inside the reserved default ASEG window.

```text
SMRAM top
+----------------------------------+
| permanent identity page tables   |
| handler text/rodata/data/BSS     |
| runtime + handler configuration  |
+----------------------------------+
| staggered per-CPU SMBASE windows |
| entry at SMBASE + 0x8000         |
| save state at window top         |
+----------------------------------+
| per-CPU stacks                   |
+----------------------------------+ SMRAM base
```

All ranges are checked for overflow, alignment, ordering, containment, and
overlap before writes. The SMRAM top and permanent CR3 are page-aligned, and
every permanent handler, entry, stack, runtime, and table address is below 4
GiB because the entry stub uses 32-bit intermediates and the identity map
covers only low memory. The default ASEG remains reserved on cold boot and S3
resume.

## Save-state formats

Save-state memory is hardware-owned. `X86SaveState` stores only a raw top
address and the selected format; it uses volatile byte transfers and never
creates a Rust reference or slice over the save-state area. The only
operations are reading the revision and writing the relocated SMBASE.

The CPU model driver selects the format through `SmmCpu`:

- Core 2 and Pineview/Atom drivers select the Intel EM64T100/101 layout and
  accept revisions `0x30100` or `0x30101`.
- QEMU's emulated CPU selects the AMD64 layout and requires revision
  `0x20064`.

Relocation validates the revision and writes exactly the selected format's
SMBASE field. An unknown or mismatched revision aborts installation.

## SMRR and IA32_FEATURE_CONTROL

The CPU model driver also names its SMRR register pair through `SmmCpu`:
Core 2 model 0Fh and Atom model 1Ch use the alternative `0xa0/0xa1` pair,
Core 2 model 16h the architectural `0x1f2/0x1f3` pair, and QEMU none.

In `init_cpu`, those drivers lock `IA32_FEATURE_CONTROL` using coreboot's
defaults: VMX outside SMX is enabled when CPUID reports VMX, the alternative
SMRR enable bit is set when the pair needs it and `IA32_MTRR_CAP` reports
SMRR, and the register is locked. A register already locked is left alone.

SMRR is best effort, as in coreboot:

- if TSEG is not a naturally aligned power of two below 4 GiB, SMRR is
  disabled with a warning;
- a CPU whose `IA32_MTRR_CAP` lacks SMRR, or whose alternative pair was not
  enabled before `IA32_FEATURE_CONTROL` was locked, skips SMRR and is counted
  in a warning.

Once SMRR is valid, normal-mode reads of TSEG return a fixed value, so nothing
reads SMRAM from normal mode after relocation.

## Lifecycle

For each SMM-capable platform:

1. Run CPU-only `mp_init()` and retain its `MpHandle`. CPU model drivers set
   `IA32_FEATURE_CONTROL` here. MP rejects a CPUID CPU count above its total
   capacity; the broadcast-SIPI trampoline parks responders beyond the AP
   limit before stack selection. Partial AP check-in is boot-fatal before any
   flight-plan barrier opens, so a late AP cannot outlive borrowed MP data
   (coreboot reports the same condition as an MP error). The static AP stack
   and mailbox budget is generated from `FSTART_MP_MAX_CPUS` by `fstart-arch`:
   Intel board plans pass their `max_cpus`, and Q35 reserves 256 slots. A
   standalone arch build defaults to 64. This is a build-time resource budget,
   not an architectural 64-CPU limit; SMM's per-CPU layout uses the same bound.
2. Discover and open TSEG.
3. Install initialized handler bytes, zero BSS, write the runtime block and
   handler configuration, and build permanent SMRAM page tables.
4. Quiesce PM1, GPE, alternate-GPI, and chipset SMI sources without changing
   `SCI_EN`, then publish the temporary shared relocation stub. Relocation uses
   only LAPIC self-SMIs, so no chipset event can enter the shared stack window.
5. Use `MpHandle::scope().scatter()` to relocate every online CPU through the
   shared default SMBASE, serialized across trigger and callback completion.
   The callback runs on the relocating CPU, checks its full LAPIC ID against
   the published one, writes SMBASE, and programs SMRR when usable.
6. Abort on lock timeout, callback timeout, unexpected CPU, save-state
   revision mismatch, or SMRR read-back mismatch. A timeout or callback
   mismatch leaves the persistent relocation bridge locked so a late callback
   cannot consume a newer CPU's target.
7. Preserve `SCI_EN` on S3 resume or clear it on cold boot, then close SMRAM,
   enable permanent SMI sources, and lock SMRAM. Any earlier failure closes
   SMRAM and is boot-fatal.
8. Send one self-SMI on every CPU. This exercises each permanent entry stub
   and handler; a broken entry stops the boot here rather than at the first OS
   SMI. It proves only that the SMI returned.

Q35 logs the selected AMD64 revision and the relocation count. CI requires
revision `0x00020064`, four relocations, SMRAM lock, and the permanent SMI
round trip in the SMP4 boot.

## coreboot compatibility output

A board can request coreboot module-argument blocks and a generated C header
with the image-relative offsets (`FLAG_COREBOOT_MODULE_ARGS`,
`FLAG_COREBOOT_HEADER`). This lets a coreboot SMM loader copy fstart's entry
stubs and handler without treating the image as an rmodule. It is not used by
fstart's own installer beyond filling the module-args block when present.

## Validation targets

- Host tests cover FSMM range validation, BSS zeroing, typed handler
  configuration placement, permanent SMRAM CR3 placement, relocation audit,
  SMRR range encoding, rendezvous ownership, layout separation, and
  save-state SMBASE offsets.
- Release-build the SMM unit and full image for Q35 and every Intel family.
- Q35 TCG SMP boots must reach payload handoff after reporting AMD64 revision
  `0x20064`, all-CPU relocation, SMRAM lock, and the permanent SMI round trip.
- D945GCLF, D41S, and X61 require hardware smoke and S3 validation; emulation
  cannot prove their chipset lock, SMRR, and cache behavior. D41S in
  particular must confirm that the TSEG owner lock completes with SMRR set.
