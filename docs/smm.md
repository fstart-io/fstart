# SMM

Reference for fstart's System Management Mode support: the standalone SMM
handler image, its installation into SMRAM, and the chipset and CPU support
it uses. Covers x86 only.

## Components

| Path | Role |
|------|------|
| `crates/smm` (`fstart-smm`) | Image header, SMRAM layout math, installer, runtime ABI, `smm_bin!`, rendezvous, save-state access |
| `crates/image-build/src/smm_image.rs` | Links the handler and writes the `FSMM` image; audits it; emits the optional coreboot header |
| `crates/image-build/asm/entry_stub.S` | Per-CPU entry stub, assembled by `crates/image-build/build.rs` |
| `crates/core/src/smm.rs` | `SmmConfig` (board `smm` setting) |
| `crates/arch/src/x86/cpu/intel/smm.rs` | `IntelSmm` installer; traits `SmramControl`, `SmiControl`, `SmmCpu` |
| `crates/arch/src/x86/cpu/intel/smrr.rs` | SMRR register pairs |
| `crates/arch/src/x86/cpu/intel/feature_control.rs` | `IA32_FEATURE_CONTROL` policy helper |
| `crates/arch/src/x86/cpu/intel/core2_cpu.rs`, `crates/arch/src/x86/cpu/intel/pineview/mod.rs` | CPU-model SMM facts (`impl SmmCpu`) |
| `crates/arch/src/x86/cpu/amd/smm.rs` | `AmdSmm` installer; trait `SmiControl` |
| `crates/driver-intel/src/gmch.rs` (`smram` module) | GMCH SMRAM register bits |
| `crates/driver-intel/src/southbridge/smi.rs` | `IchSmi` (SMI control), `PendingSmi` (one SMI's sources), `handle`, `IchSmmConfig` |
| `crates/driver-amd/src/fch/smi.rs` | `FchSmi` (AMD FCH SMI control), `PendingSmi`, `Sleep`, `handle` |
| `crates/platform-qemu/src/q35_smm.rs` | Q35 TSEG/SMRAM control (MCH), `QemuSmmCpu`, ICH9 PM block |
| `crates/platform-sp3/src/smm.rs`, `src/smm/` | SP3 handler, `Sp3Smi`, native RAS observer |
| `boards/<vendor>/<board>/src/smm.rs` | The board's SMI handler: `smm_bin!(handler: …)` |

The `smm_bin!` macro is `fstart_smm::smm_bin!`, re-exported by each platform
crate as `smm` (for example `fstart_platform_intel::smm::smm_bin!`).

## Build

The SMM handler is a separate unit named `smm`. It is release-only.

```sh
cargo fbuild build --board qemu-q35 --stage smm --release
cargo fbuild build --board tyan-s8030 --stage smm --release
```

`--stage <unit>` builds the named unit and its producers. A full build
(`cargo fbuild build --board <board> --release`) builds the `smm` unit first.
fbuild passes the resulting image to the stage as `FSTART_SMM_IMAGE`, and the
stage embeds it (`include_bytes!`). Stages built without that variable have no
SMM image: MP setup skips SMM and SMRAM stays unlocked.

Board settings (`crates/core/src/smm.rs`, `SmmConfig`):

| Field | Default | Notes |
|-------|---------|-------|
| `entry_points` | Intel: `max_cpus`; Q35: 8; SP3/noncar: `MAX_CPUS` | Number of precompiled entry stubs. The Intel plan validator rejects a value below the MP CPU ceiling. |
| `stack_size` | `0x400` | Per-CPU SMM stack. SP3 uses `0x2000`. |
| `coreboot.emit_header` | `false` | Emit `offsets.h` (see below). |
| `coreboot.module_args` | `false` | Add coreboot module-argument blocks. |

Q35 hardcodes 8 entries in `crates/platform-qemu/src/host.rs`. When an SMM
image is present, Q35 MP bring-up uses the image's entry count as its CPU
capacity, so an SMM-enabled Q35 build supports at most 8 online CPUs.

## Handler image (`FSMM`)

`crates/smm/src/header.rs` defines the format. There is no version field; the
builder and installer must come from the same tree.

All integers are little-endian. `handler_offset`, `entries_offset`, and
`stub_offset` are file offsets. Every other handler offset is relative to the
copied handler memory base.

| Field | Meaning |
|-------|---------|
| `magic` | `FSMM` (`SMM_IMAGE_MAGIC`) |
| `header_size`, `entry_desc_size` | Must match this build's struct sizes |
| `flags` | bit 0 `FLAG_COREBOOT_MODULE_ARGS`; bit 1 `FLAG_COREBOOT_HEADER`; bit 2 `FLAG_PROTECTED_MODE` (i686 handler) |
| `image_size` | Total file size |
| `entry_count`, `entries_offset` | Array of `EntryDescriptor` |
| `handler_offset`, `handler_load_size` | Initialized handler bytes in the file |
| `handler_mem_size` | Full in-SMRAM extent: handler, runtime, config, module args |
| `handler_entry_offset` | `fstart_smm_handler` |
| `runtime_offset`, `runtime_size` | `SmmRuntime` block |
| `handler_config_offset`, `handler_config_capacity` | Config slot; capacity is exactly 256 bytes, 16-byte aligned |
| `module_args_offset`, `module_args_size` | Coreboot module-argument array, or 0 |
| `stack_size` | Per-CPU stack size |
| `fixups_offset`, `fixup_count` | i686 only: handler offsets of 32-bit absolute addresses to rebase |

`EntryDescriptor` is `stub_offset`, `stub_size`, `entry_offset`, `params_offset`.
`params_offset` is the position of `SmmEntryParams` inside its stub.

File layout written by `build_image`: header, descriptors, handler bytes,
entry stubs (each `stub_size` bytes, 16-byte aligned), then fixups.

### Handler link and audit

`handler_from_archive` and `handler_from_rlibs` link with `ld -T` and
`--emit-relocs`, with `fstart_smm_handler` as entry. The linker script places
`.text`, `.rodata`, `.data`, and `.bss` (NOLOAD) at VMA 0, each 16-byte aligned.
The audit in `smm_image.rs` rejects:

- Allocated sections other than `.text`, `.rodata`, `.data`, `.bss`.
- Section alignment that is not a power of two, is above 4 KiB, or is not met by the VMA.
- Undefined symbols and Rust panic symbols (`panic_fmt`, `unwrap_failed`, …).
- x86_64: any relocation other than PC-relative; GOT/PLT-indirect calls. A PC-relative target must resolve inside the copied image. The audit accounts for x86 next-RIP bias.
- i686: absolute 32-bit relocations are recorded as fixups. Each must point inside the copied image.

Loader behavior:

- Handler memory is loaded at a 4 KiB boundary (`SMM_HANDLER_ALIGNMENT`).
- x86_64 handlers are not patched. A single load delta preserves all cross-section references.
- i686 handlers are rebased by adding the handler base to each fixup.

Mode: `SmmArch::from_target` picks long mode for `x86_64*` targets and
protected mode otherwise. The installing stage must match the handler's mode
(`InstallError::ModeMismatch`). Coreboot header or module-arg output is not
allowed for protected-mode handlers.

### Coreboot output

With `coreboot.emit_header`, fbuild writes `offsets.h`: `FSTART_SMM_*` defines
for the offsets above, plus `FSTART_SMM_HANDLER_ALIGNMENT` (4096). Coreboot
loaders can copy the entry stubs and handler from these offsets.

## SMRAM layout

`compute_cpu_layout` and `compute_common_base` in `crates/smm/src/layout.rs`
place everything inside the permanent SMRAM/TSEG range:

```text
SMRAM top
+------------------------------+
| identity page tables (6 pg)  |  4 GiB identity map, 2 MiB pages
+------------------------------+
| handler text/rodata/data/bss |  4 KiB aligned
| runtime, config, module args |
+------------------------------+
| SMBASE windows (64 KiB each) |  entry at SMBASE+0x8000
|                              |  save state at window top
+------------------------------+
| per-CPU stacks               |  stack i at base + i*stack_size
+------------------------------+ SMRAM base
```

- SMBASE values are staggered by `max(save-state size, stub size)` (0x400 for the usual stubs) inside each 64 KiB segment. Segments grow downward from below the handler region.
- Stub length must be below 0x8000.
- The installer checks overflow, alignment, containment, and overlap before any SMRAM write. SMRAM top and page-table base must be 4 KiB aligned.
- Every permanent address must be below 4 GiB, because stubs use 32-bit intermediates and the tables map only low memory.

Temporary default-SMBASE relocation (`SMM_DEFAULT_SMBASE` = `0x30000`), inside
the architectural ASEG window (`SMM_DEFAULT_ASEG`, `0x30000..0x40000`):

| Address | Use |
|---------|-----|
| `0x30000 + 0x7000` | Stack top for the relocation stub |
| `0x30000 + 0x8000` | Relocation entry stub |
| `0x30000 + 0x9000` | Relocation identity tables (6 pages) |

The ASEG window must be excluded from OS-visible RAM (`mp.rs` documents this).

## Installation (`install_pic_image`)

All validation (header, descriptors, mode, layout, addresses below 4 GiB) runs
before the first SMRAM write. Then the installer zeroes `handler_mem_size`,
copies the handler, applies i686 fixups, writes `SmmRuntime` and the config,
builds the page tables, and copies each stub and patches its `SmmEntryParams`.
`install_default_relocation_callback_stub` does the same for the default-SMBASE stub.

## Runtime ABI

`SmmRuntime` (`crates/smm/src/runtime_abi.rs`): `smram_base`, `smram_size`,
`num_cpus`, `stack_size`, `handler_config_offset`, `handler_config_size`, and an
atomic `owner` word for the rendezvous. It holds no pointers and no per-CPU tables.

`SmmEntryParams` is loader-filled data inside each stub: `cpu`, `stack_size`,
`stack_top`, `common_entry`, `runtime`, `coreboot_module_args`, `cr3`, and
`entry_base`. The stub uses `entry_base` rather than `CS`, because the visible
selector can be truncated for high SMBASE values.

### Board handler and config

The board owns its SMI handler, a function the board's `smm.rs` binds:

```rust
fn smi_handler(ctx: &mut SmmContext<'_>, config: &IchSmmConfig) { … }
fstart_platform_intel::smm::smm_bin!(handler: smi_handler);
// or, with CPU-local work on every entrant:
smm_bin!(handler: smi_handler, enter_cpu: enter_cpu);
```

A handler reads the chipset's sources once (`PendingSmi::take`), acts on the
ones it owns, and ends the SMI (`PendingSmi::finish`). Boards without SMI
sources of their own bind the chipset's default (`smi::handle`).

- `Config` is the type the handler takes. The installer writes it into a
  256-byte, 16-byte-aligned slot. The handler borrows it in place
  (`SmmContext::handler_config`), and only if the recorded size equals
  `size_of::<Config>()` and the slot lies inside SMRAM. Otherwise neither
  `enter_cpu` nor the handler runs.
- The southbridge names the same type as `SmiControl::HandlerConfig`. For ICH
  that is `IchSmmConfig` (`pm_base`, `gpe0` geometry).
- `enter_cpu` runs on every entrant before the rendezvous. Use it for CPU-local
  work such as MCA capture. It must not wait for other CPUs.

`fstart_smm_handler` (generated by `smm_bin!`) calls `fstart_smm::handle`, which
runs `enter_cpu`, then the rendezvous, then the handler on the owner only.

### Rendezvous

`enter_rendezvous` claims `SmmRuntime.owner` with CAS (value = CPU + 1). The
owner runs `handle`; other CPUs spin with `pause` until the owner releases it,
then return through `rsm`. Ownership is a plain atomic in the TSEG runtime block.

## Entry stub

`entry_stub.S` starts in 16-bit real mode at `SMBASE+0x8000`, enters protected
mode, and (x86_64 build) enables paging with `cr3` and long mode. It then sets
`rsp` from `stack_top`, calls `common_entry(params)`, and executes `rsm`. The
i686 build (`-DFSTART_PROTECTED_MODE`) stays in flat 32-bit protected mode without
paging and passes the params pointer as a cdecl argument.

## Save-state access

`X86SaveState` (`crates/smm/src/save_state.rs`) wraps the exclusive top of a
CPU's 64 KiB SMBASE window. All reads and writes are volatile byte transfers.
No Rust reference or slice is formed over hardware-owned memory.

The revision word is at `top-0x104`. SMBASE offsets depend on the format:

| Format (`X86SaveStateFormat`) | Accepted revisions | SMBASE field | Selected by |
|-------------------------------|--------------------|--------------|-------------|
| `IntelEm64t` | `0x30100`, `0x30101` | `top-0x108` | Core 2 (non-Yonah), Pineview |
| `IntelLegacy` | bit 17 set, not AMD64 | `top-0x108` | Yonah (Core Duo) |
| `Amd64` | `0x20064`, `0x30064` | `top-0x100` | QEMU, CPUs with long mode |
| `Amd64OrIntelLegacy` | either of the above | by revision | QEMU, CPUs without long mode |

`write_smbase` validates the revision and writes only the SMBASE field. A
mismatch returns `SaveStateMismatch` and aborts installation.
`read_amd64` / `write_amd64` access saved RAX, RCX, RDX, and RIP (AMD64 only);
the SP3 RAS observer uses them.

## Intel installation (`IntelSmm::install`)

`IntelSmm` (`crates/arch/src/x86/cpu/intel/smm.rs`) is composed from:

| Trait | Implemented by | Provides |
|-------|----------------|----------|
| `SmramControl` | Northbridge (Q35: `Q35HostBridge`) | `tseg()`, `smram_open()`, `smram_close()`, `smram_lock()` |
| `SmiControl` | Southbridge (`IchSmi`) | `HandlerConfig`, `set_acpi_mode`, `quiesce_for_relocation`, `enable_permanent_smi`, `lock_permanent_smi` |
| `SmmCpu` | CPU-model driver | `smm_save_state_format()`, `smrr_pair()` |

Flow:

1. `mp_init` brings CPUs online and runs each CPU driver's `init_cpu` (which sets `IA32_FEATURE_CONTROL`). `IntelSmm::install` takes the `MpHandle` and reads the online count from it.
2. Read TSEG; zero size is `TsegDisabled`. CPU count above `MAX_CPUS` is `TooManyCpus`.
3. `smram_open()` must confirm D_OPEN and G_SMRAME with D_LCK clear; otherwise close and fail (`SmramOpenFailed`).
4. Compute the SMRR range if the CPU supports SMRR. Run `install_pic_image` into the open TSEG.
5. `quiesce_for_relocation()` clears chipset SMI sources and sets only the global SMI gate and EOS. SCI_EN is untouched. Install the default-SMBASE stub and tables, and write back TSEG and ASEG from cache.
6. Relocate every CPU through `MpHandle::scope().scatter` (APs first, BSP last). Each relocation is serialized by a bridge lock, then:
   - the CPU being relocated sends a LAPIC self-SMI to itself;
   - the callback checks the full LAPIC ID, writes SMBASE, and programs SMRR when it applies;
   - the CPU waits for completion.
   Lock or completion timeouts, an unexpected CPU, a revision mismatch, or an SMRR failure poison the bridge, so no later target is published.
7. Erase the ASEG window. Call `set_acpi_mode(resume)`, `smram_close()`, `smram_lock()` (verifies D_LCK, D_OPEN clear, G_SMRAME, T_EN), `enable_permanent_smi`, then `lock_permanent_smi()` (verifies SMI_LOCK and GBL_SMI_EN). Any failure is fatal.
8. Send one self-SMI on every CPU. This runs each permanent entry once at boot.

Log lines are prefixed with the platform name (`Q35 SMM:`, …).

## SMRR and `IA32_FEATURE_CONTROL`

`SmrrPair` (`smrr.rs`):

| Pair | Registers | Notes |
|------|-----------|-------|
| `Architectural` | `0x1f2` / `0x1f3` | Base encodes memory type 6 (write-back) |
| `Core2Alternative` | `0xa0` / `0xa1` | Usable only when `IA32_FEATURE_CONTROL` is locked with `SMRR_ENABLE` |

CPU-model SMM facts:

| CPU driver | Save state | SMRR pair | `IA32_FEATURE_CONTROL` in `init_cpu` |
|------------|-----------|-----------|--------------------------------------|
| Core 2, model 0Fh | `IntelEm64t` | `Core2Alternative` | Lock with VMX-outside-SMX (if VMX) and `SMRR_ENABLE` (if MTRRCAP.SMRR) |
| Core 2, other models (e.g. 16h) | `IntelEm64t` | `Architectural` | Same as above (`feature_control_bits()` is 0 for `Architectural`) |
| Yonah (Core Duo) | `IntelLegacy` | none | Not touched |
| Pineview (Atom 1Ch) | `IntelEm64t` | none | Lock with VMX-outside-SMX (if VMX) |
| QEMU (`QemuSmmCpu`) | `Amd64` or `Amd64OrIntelLegacy` | none | n/a |

`enable_and_lock` (`feature_control.rs`) skips the MSR unless CPUID reports
VMX or SMX, or the SMRR enable bit is requested. A register that is already
locked is left alone. Pineview's source notes that coreboot's Core 2 pair left
the D41S unable to complete locked TSEG accesses, so Pineview reports no pair.

Per-CPU SMRR behavior during relocation:

- CPU supports SMRR (`MTRRCAP.SMRR`): program the TSEG range and read back. An invalid range (not a power of two, unaligned, or above 4 GiB) or a failed read-back is `SmrrSetupFailed`. A `Core2Alternative` pair that is not locked with `SMRR_ENABLE` is also `SmrrSetupFailed`.
- CPU lacks SMRR: no SMRR access. Relocation proceeds. If the model has a pair, a warning reports how many CPUs got SMRR.
- Chipset SMRAM and SMI locks apply regardless of SMRR.

## Intel SMI handler (ICH)

`IchSmi` (`crates/driver-intel/src/southbridge/smi.rs`) implements `SmiControl`
for ICH7–ICH10. The register layout differs only in GPE0:

| Layout | GPE0 status |
|--------|-------------|
| `ICH7_GPE0` | 32-bit at PMBASE+0x28 |
| `ICH8_GPE0` | 64-bit at PMBASE+0x20 (ICH8–ICH10, Q35) |

Permanent SMI sources after `enable_permanent_smi`: TCO, APMC, global SMI, and
EOS, plus PM1 power button and any GPE/GPI enables restored from the saved
state. `SLP_SMI_EN` is not set.

`PendingSmi::take` reads `SMI_STS` and the sources it reports:

- APMC port (`0xb2`): `0x1e` clears SCI_EN, `0xe1` sets it. The command is consumed only when `APM_STS` is set and lands in `apm`; `0xcb` (finalize) is filtered out.
- TCO SW SMI: the command lands in `tco`; the board answers with `tco_reply`.
- GPE0 status lands in `gpe`; alternate GPI events (cleared) in `gpi`.

`finish` resets TCO status, clears PM1 and GPE0 status and `SMI_STS`, and sets
EOS. The ThinkPads handle their EC and dock this way; the other boards bind
`smi::handle`.

## AMD64 installation (`AmdSmm::install`)

AMD CPUs take TSEG from `SMM_ADDR`/`SMM_MASK` and SMBASE from `SMM_BASE`. These
are writable from normal mode until `HWCR.SmmLock`. No default-SMBASE stub,
relocation SMI, or save-state write is used.

`AmdSmm` (`crates/arch/src/x86/cpu/amd/smm.rs`) uses one trait, `SmiControl`
(`disable_smi`, `enable_smi`, `smm_handler_config`), implemented by
`fstart_driver_amd::fch::smi::FchSmi`.

1. Check TSEG: power of two, at least 128 KiB, size-aligned, below 4 GiB. Check that `HWCR.SmmLock` is clear (`Locked` otherwise). Check CPU count.
2. `disable_smi()`. On the BSP, clear TValid/AValid in `SMM_MASK`. Zero TSEG, run `install_pic_image`, write back TSEG.
3. Scatter over CPUs (APs first, BSP last). Each CPU writes `SMM_ADDR`=TSEG base, `SMM_MASK`=TSEG mask over every CPUID physical-address bit (write-back, TValid, no ASEG), `SMM_BASE`=its SMBASE, and sets `HWCR.SmmLock`. Each reads back; a mismatch is `MsrSetupFailed`.
4. `enable_smi()`, then one self-SMI per CPU.

Returns `InstalledTseg { base, mask, free }`. `free` is the zeroed TSEG
between the stacks and the lowest SMBASE window. `platform-amd`'s
`noncar/psp.rs` places the PSP SMM info buffers there.

This is a cold-boot path. There is no resume argument. An already-locked
`HWCR` is an error.

## Limits

- The Q35 SMM image has 8 entry stubs (see Build). The installer returns `NotEnoughEntries` for more online CPUs.
- The Intel path passes `resume` from the caller. Q35 passes `false`.
- The AMD path has no S3 handling (see above).
- The format is unversioned. Build and install from the same source tree.

## Related

- [`amd-sp3-ras.md`](amd-sp3-ras.md): native SP3 SMM RAS observer, built with `cargo fbuild build --board tyan-s8030 --stage smm --release`.
- [`pineview-boards.md`](pineview-boards.md): Pineview board notes.
