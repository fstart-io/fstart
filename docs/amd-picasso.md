# Native AMD Picasso / Google Vilboz port

## Status: a foundation and cold-boot console checkpoint, NOT a complete port

This change is **not hardware-booted, not OS-bootable and not flash-ready**.
It has no FSP-M/FSP-S dependency or hidden binary call. It also does **not**
replace those binaries' work with no-ops and claim a successful native port.
There is deliberately no production `fbuild` metadata for this board yet.
The common build-plan/image executor needs an AMD packaging projection before
this can become a supported board. The checkpoint is built explicitly below.

The implementation is in jj workspace `../fstart-picasso`. Audited foundation
`qtyqotpq` is a child of the user's `mpzspnrw`; continued native work is in its
child `vsvkyzmk`. All three reference/original workspaces are untouched.

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
| Permanent MP stacks/IDTs and page padding | `02050000..02061000` |

The low AP bootstrap/handler page `00008000..00009000` is also permanently
reserved. These firmware reservations must survive payload handoff until reset.

The linker explicitly uses these **DRAM VMAs/LMAs**, not `ff000000` flash VMAs.
The PSP BIOS-directory entry type `62` copies an uncompressed 128-KiB image to
`02030000`; reset=1, copy=1, compressed=0. Reset enters the final 64-KiB segment,
loads a 32-bit-base GDT (the DRAM address exceeds 16 MiB), builds identity page
tables and enters 64-bit Rust on a DRAM stack. Cache remains disabled for this
checkpoint, so FCH accesses cannot accidentally be write-back cached.

The linker emits an RX reset-image segment, and entry code never mutates it.
The current large-page identity mapping does not enforce per-image write
protection; preserving its bytes is a firmware invariant. GDT descriptors
pre-set Accessed so CPU segment loads do not implicitly modify reset-image
bytes; this is verified in the linked ELF, not just assembly text. Initialized writable
`.data` is deliberately rejected by a linker assertion. BSS, stack and page tables
are separate from the image, APOB and PSP work buffer. This preserves the
PSP reset-image integrity constraint; it does **not** implement S3 resume.
The linker reserves the complete early window for a future OS memory map.

## Source coverage and remaining work

**Complete native silicon integration is still required.** The original
Picasso tree delegates it to opaque FSP routines, but the separate native
Picasso checkout provides recovered implementations. Those are now being
ported to Rust rather than accepting FSP as a permanent boundary. PSP-trained
DRAM alone is not proof that PCIe, graphics, security or power initialization
is complete.

### Continued native implementation

The new driver modules implement the SMN and DF indirect protocols, a bounded
six-word SMU mailbox, checked APOB memory-map parsing, CPU/MCAX sequencing,
AMD-specific fixed/variable MTRRs and all ordered NBIO base tables. The platform
`early::initialize` composes these mechanisms with a native memory/resource
contract; all APOB holes (including unknown kinds), PSP/early reservations,
TSEG, UMA and alignment slack remain outside OS-allocatable RAM.

Native DXIO now implements the engine/ancillary wire protocol, state-machine
handshakes, PCIe remapping/equalization and pre/post-training register sequences.
Its DMA buffers are a consumed static exclusive borrow: an error does not
prove DMA quiescence, so their mapping/storage must survive until reset.
The driver also implements ACP shutdown, eMMC modes/presets, FCH A-Link/clocks,
USB controller/PHY/OC sequencing, late PCIe power/CLKREQ and persistent SMU
policy/scratch pages. Board native typed data includes ASPM, USB tuning,
EC-selected telemetry and correct clock-request wire codes (pin 0 is not
Disabled). Topology APIs return cold-policy reset requirements and release
AP ThreadEn slots; that is not an implemented AP rendezvous. Legacy mechanisms
cover RTC/internal IRQ/LPC policy and byte-width ROM-sharing disable; exception
handlers must exist before native RTC accesses enable NMI.

Both power pages and DXIO buffers must remain coherent, mapped and permanently
reserved from payload/allocator memory, including after errors. Late SMU
nonstandard responses are surfaced diagnostically only in source-backed policy
callers; early/DXIO/USB requests remain strict, and all-ones readiness fails
before submission.

These APIs are not yet connected to a complete boot binary. The console
checkpoint remains deliberately limited while FCH/eSPI/EC integration,
MP/SMM, graphics, tables, stage loading and supported packaging are completed. The independent
foundation audit found two API bugs: unsupported both-edge SCI and nonzero
APCB subprograms creating duplicate generated backup selectors. Both now fail
before hardware/image writes, with focused negative tests. Continued source
comparison also corrected PM_ACPI_CONF to byte offset 0x74 and made the FCH
constructor exclusive/private. The independent native-early audit found a
Raven2 late-DF mask error (bit 11 was cleared then re-set); it now sets 0x3300
rather than 0x3b00 for Raven2, with both-variant regressions. The subsequent
DXIO/ACP/eMMC audit initially found no further issues. A broader independent
audit subsequently found and closed all-ones SMU readiness, odd SDHCI divider
rounding and missing L1 permission for combined L0s/L1. The last two defects
were inherited from native C: literal source matching was insufficient.
Representable divider/rate tests now preserve the 400-kHz Vilboz preset while
rounding 12 MHz and 300 kHz safely; an explicit Minimum variant represents
Dalboz's slowest-clock sentinel. No bounded audit approves full-flow integration
or hardware behavior. A follow-up independent audit verified legacy/eSPI
mechanisms and whole power-transfer regression without new findings.
The subsequent shared ChromeEC driver and distinct legacy board-ID correction
also passed independent review. LPC/SPI/SMBus and AOAC changes passed review
after correcting SPI busy preflight to STATUS0x4c.

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

### Shared ChromeEC transport

`crates/driver-chromeec` implements no-alloc protocol-v3 packets over linear
LPC/eSPI I/O, with explicit little-endian zerocopy headers, bounded waits and
validated response lengths/version/reserved/checksum/result. Submitted timeout
or malformed response poisons its owner rather than duplicating a command;
IN_PROGRESS does not permit a silent retry. Caller output is published only
after validation. This is not a MEC EMI or RTK backend: Zork's supplied Kconfig
selects ordinary eSPI/LPC and does not select those variants.

Board fact acquisition composes the CBI FW_CONFIG lower word with optional
SSFC in the upper word, preserving valid zero and failure fallbacks. It is not
a single 64-bit CBI read. Legacy GET_BOARD_VERSION (command6, two-byte LE)
is acquired independently: Vilboz's external-audio-clock/eDP quirk uses this
board_id()>2 and rejects unknown, while GPIO/WiFi predicates use CBI tag0.
No equality between these sources is assumed; crossed-value tests enforce it.
These APIs are tested but not yet invoked by the
checkpoint or a complete boot binary; eSPI decode/pads must be ready first.

### Shared MP rendezvous work

Native Picasso MP remains unwired and unsafe to activate without the target
cache/reset/event/residency contracts. The architectural scaffold now supplies
bounded exception state and a platform-selected launch seam, not a full port.
Its flight-timeout path also previously opened barriers and could expire
borrowed CPU drivers while AP initialization still ran. The current CPU-phase
change keeps timed-out barriers closed and takes a non-returning failure path;
a guard preserves driver borrows during BSP unwinding too. Platforms can supply
an explicit non-unwinding reset/fail-stop handler. The default stops the BSP
without claiming AP/SMM quiescence. Independent source review closed this
lifetime defect; it does not implement native AP startup or close other gates.
The AP trampoline additionally normalizes CR0.EM/TS/MP/NE and CR4 SSE flags,
initializes x87 and loads reset MXCSR before entering compiled Rust. Emitted
protected-mode instructions were independently reviewed, closing that defect.

The next cache change captures an ordered, bounded LE MSR image into the resident
trampoline: MTRRs disabled, vendor prefix, PAT/fixed/variable pairs, vendor suffix
and final default type. Independent review accepted this bounded mechanism,
not Picasso integration: an AP-writer mutex alone does not keep BSP/siblings
cache-off during shared-MSR updates.

A new opt-in coordinated image requires BSP CD=1/NW=0, cache writeback and
PGE/TLB invalidation before launch. Every intended AP must announce actual
cache-off readiness before any image writer runs; each stays cache-off after
restoration. Only all-ready/all-restored completion permits the BSP's final
cache/TLB cleanup and uncached release publication. Timeout keeps release
closed and the BSP cache-off, with no Drop-based re-enable or retry. Restoration
is separate from Rust check-in and flight completion. Picasso preparation
selects this protocol; independent Intel/virtual paths remain unchanged (some
Intel platforms cannot execute WBINVD with CD=1). The coordinated BSP requires
interrupts disabled, PCID off, and an initially cache-enabled validated policy.

Independent review accepted the bounded software protocol under its stated
contracts; native cache P1 is not closed and no hardware execution is claimed.
Target WBINVD/CD and MSR semantics, the complete intended CPU set, exclusion of
SMI/NMI handlers, and subsequent shared-policy writes still require proof.
Official AMD OSRR downloads failed. A mirror of vendor document56255 rev3.03
(July2018, Models00h–2Fh) is now retained in full: it documents core-shared
MTRRs/TOM/TOM2/SYSCFG, per-thread PAT, and the per-thread exception for SYSCFG19.
This is newly read register-scope evidence, not cache-transition/reset/SMU or
hardware acceptance; exact revision/errata applicability remains required.
Search synthesis and the unrelated Models01h/08h PPR are not target authority. Architectural capture is Intel-only by default; AMD
must explicitly supply its policy, never GenericX86CpuDriver's fixed bytes.
Picasso preparation exposes RdDram/WrDram and preserves TOM/TOM2/SYSCFG/PAT;
tests cover wire order, rejected geometry and restoration of the BSP read gate.
This preparation is not yet wired to a native CPU driver/SMU launch. Cache/SMT
scope, actual coherent mappings, exception handling and residency still require
integration review. Bootstrap CR3/stack geometry now rejects 32-bit truncation
and arena wrap. A platform-owned-entry feature prevents the generic Intel/XIP
reset and ram entries from colliding with PSP DRAM startup; utilities remain
available. None of this establishes actual AP execution or a stack budget.

Independent review closed the bounded AP-IDT-before-LINT1 defect. The exception
change supplies two zero-initialized resident tables:
256 protected-mode gates and 256 long-mode gates (6144 bytes total). Both point
to assembly-only terminal handlers in the copied trampoline, with no logging,
Rust calls, unwind or return. Protected LIDT follows private-stack assignment
and precedes SSE/MSR work; long-mode LIDT precedes compiled AP code and LAPIC
LINT1/NMI unmasking. The gates use mode-specific selectors and the current AP
stack (IST=0). They do not claim emergency stacks, TSS support, stack exhaustion
protection, or SMM isolation. The pre-stack and paging/mode-switch windows still
require platform asynchronous-event exclusion and valid mappings/control state.
BSP exception handling is a separate obligation. MP initialization is now a
single attempt per boot, so live tables/trampoline cannot be overwritten by a
retry; initialized writable ONLINE_CPUS storage was replaced with zero BSS.
The additional table residency/reservations and a fully linked native MP image
remain integration gates; the checkpoint still does not invoke MP.

The next launch change adds sealed `PreparedAps` ordering metadata and
`mp_init_with_ap_launcher`: selected count validation uses cached platform facts
before MP ownership/hardware/publication, even with zero APs. The prior zero-AP
bypass was independently found; the correction rejects eight-thread/BSP-only
mismatch without hardware effects and permits a genuine one-thread topology
without SMU submission. Actual release still requires resident bootstrap/table/
configuration publication and selected BSP cache-off entry. Custom submission
is one-shot; neither a second custom submission nor INIT/SIPI fallback occurs
on failure or timeout. The ordinary INIT/SIPI path retains its second SIPI.
`platform-amd::mp::ColdRelease` adapts the existing SMU ThreadEn driver, requires
a complete intended set and coordinated cache image, and preserves typed
failure diagnostics. All-ones ThreadEn is now unavailable, not a claimed
already-released set. Unsafe construction requires validated cold-reset/BSP-slot0,
cache, timing, mapping and asynchronous-event facts; it establishes none of them.

The PSP reset image now contains immutable APICBASE.BSP-bit dispatch before any
BSP stack/BSS/page-table initialization. APs far-transfer to the architectural
resident bootstrap address; the segment is derived from the shared ABI constant.
No reset-image patch/restore is performed. The platform must publish/reserve
that bootstrap before releasing APs, and verify the actual SMU reset entry/control
state. The dispatcher/release adapter and the native CPU driver below are not
an invoked complete boot flow; S3/runtime image protection remain unimplemented.

### Explicit resident CPU bootstrap storage

`arch::x86::mp::BootstrapStorage<N>` supplies N private AP stacks and both IDTs
in platform-owned permanent RAM, rather than the default stage BSS allocation.
Use its const constructor in a reserved NOLOAD static, not on the BSP stack.
`mp_init_with_resident_storage()` consumes the static mutable owner on every
result. Exclude the complete `allocation_range()` (including unused slots and
padding) from allocators/payload maps until reset; parked APs retain this memory
after callbacks finish. The IDTs are fully encoded before publication, without
copying a large initialized arena value on the stage stack.

Selected count checks remain unconditional and precede storage preflight.
Configured capacity, whole low-4GiB extent, alignment and trampoline/default-SMRAM
separation are checked before attempt claim, LAPIC, hooks or publication. These
are numerical checks only, not residency, coherency, mapping or event proofs.
The default wrappers retain their old storage budget; explicit users do not
reference that default allocation. Seven 4-KiB stacks plus IDTs need34KiB,
still larger than28KiB checkpoint BSS, so external placement is required.
A never-executed release link harness retains the real generic MP path with
seven external stacks: stage BSS is0x1030 bytes, external NOLOAD allocation
is0x8800 bytes, and the default arena is absent. This is a generic-link budget
check, not a complete native-driver binary, stack measurement, physical mapping
or boot result. The separate platform integration below does not activate MP.

Linking that path exposed16 bytes of static GOT pointers (`memcpy` and AP entry)
previously classified as writable initial data. The linker now places only the
host-resolved `.got` in the immutable RX LOAD. Runtime-link/IFUNC sections and
all genuine writable initial data remain forbidden. LLD retains SHF_WRITE on
the synthetic section; RX program-header placement is not runtime page WP.

### Platform resident-memory integration

`platform-amd::layout` and `linker/picasso.ld` allocate fifteen AP slots plus
IDTs (`0x10800` bytes), rounded to a permanent `0x11000`-byte NOLOAD window at
`02050000`. Sixteen total CPUs is a software capacity covering the topology
decoder's maximum envelope, not discovery of sixteen threads on a board. Linker
assertions bind the actual section size and endpoints to the memory-map policy.

`Memory::from_apob()` rejects holes or unavailable RAM anywhere in that window,
including unused slots/page padding, and in the low bootstrap page. It then
reserves both through reset, along with the resident microcode page below;
usable totals and the state passed to a later payload exclude these whole
allocations and the existing PSP/image/TSEG reservations.
Allocators must use this map, not just its `allocation_top` ceiling.

`mp::initialize_resident()` validates the selected complete CPU count before
any storage ownership claim, verifies map reservation and actual static
placement, then consumes the linker-owned allocation once. It never clears its
claim or returns a reclaim token. Architectural entry repeats cached preflight
before its own claim; physical mapping, coherence, event/cache/reset semantics
and retention of all other AP resources remain unsafe integration obligations.
The console checkpoint does **not** invoke this entry or a native CPU driver.
No native AP/OS execution, measured stack budget, SMM isolation or S3 is claimed.

### Native CPU callback adapter

`platform-amd::cpu::NativeCpu` implements the architectural CPU-driver interface.
Unsafe preparation requires an independently verified complete APIC-ID list with
BSP first, cached SMU topology, held APs and the existing physical/cache/event
contracts. CPUID counts and threads-per-core must agree; duplicate/unknown IDs
are rejected. **ThreadEn slots are not inferred to be APIC IDs.** OSRR56255
§2.1.8.2.1.3 explicitly permits noncontiguous assignments; Fn80000008.ECX defines
NC+1 as package threads, Fn8000001E.EBX defines ThreadsPerCore+1, and the SMT
APIC-ID layout places ThreadId in bit0. These scoped document facts do not
establish the actual board's CPU set. Fn8000001E is reserved with APIC disabled:
the adapter checks enabled xAPIC before reading it, rejects x2APIC and broadcast
ID255, and verifies BSP role separately. The already installed, locked PSP BAR
is read on the BSP and cached; APs never access SMN.

All logical CPUs serialize microcode and CCX work through one bounded,
poison-on-failure lock. Its iteration budget is not a measured-time guarantee.
The native tail programs AMD MMCONFIG, PSP_ADDR, C-state I/O, MCAX and the existing
per-core policy. It requires the restored BSP SYSCFG/TOM/TOM2/PAT/default policy
to match and performs **no late shared cache-policy writes**. TOM2En/TOM2WB must
already be established by early setup and coordinated pre-Rust restoration.
This does not close the native cache applicability/errata gate.

The driver parses a trusted, immutable flat vendor microcode patch using
zerocopy, checks its exact CPU-equivalent revision and bounded 3200-byte format,
and validates the entire aligned physical extent. This size is the observed
PCO B0/B1 and RV2 A0 input format, not a general AMD-family bound. Wrappers and
containers are not accepted. Parsing does not authenticate the vendor signature.
The whole backing allocation must remain coherent, immutable, identity-mapped
and reserved through reset. Loading verifies the installed MSR patch level,
retains an already newer revision, and never retries a rejected submission.
No vendor bytes are redistributed.

Per-CPU work bits alone do not authorize CCX-done: only the BSP post-MP hook after
the architectural callback-return barrier publishes completion. Failures halt
without unlocking poisoned ownership or withdrawing live driver borrows. A
strict production-linker probe retains actual native construction and callbacks;
it is never executed/flashed, and proves neither physical execution nor peak
stack use. The console checkpoint still invokes no native CPU/MP flow. BSP
exceptions, SMM/reset isolation, target cache/reset semantics, trusted image
integration and measured release stack budgets remain outstanding.

### Permanent microcode staging

`platform-amd::microcode` owns one const-MaybeUninit NOLOAD page at
`02061000..02062000`. The selected 3200-byte patch and the entire 896-byte unused
tail stay reserved through reset. This is firmware allocation policy, not PSP
scratch discovery or physical placement approval. APOB/TOM validation rejects
any hole/truncation/TSEG collision across the whole page before ownership; the
handoff map and usable totals exclude it even if staging/preparation fails.
Allocators must use the map, not just `allocation_top`.

`NativeCpu::prepare_staged()` validates complete supplied CPU identities/count,
BSP role, topology, patch, locked PSP BAR and routing/MCA policy before staging
claims anything. The staging entry checks patch, CLFLUSH geometry, map witness,
actual linker placement and whole-page source disjointness before its one-way
claim. It copies directly into raw permanent storage, initializes every unused
tail byte, writes back the whole page and only then publishes an immutable view.
There is no initialized page-sized BSP-stack temporary, reclaim token, mutable
view, failure unlock or retry. The original immutable reset image is untouched.

The source may be temporary ordinary memory; it must be independently trusted,
readable and immutable through copying. Parsing/copying is **not authentication**.
The caller still establishes whole-page coherent identity WB RAM, physical
ownership, asynchronous/reset/SMM/DMA exclusion and map retention through reset.
No generic WBINVD fallback is accepted for staging. Neither staging nor preparing
submits PATCH_LOADER; only the subsequent serialized native callback may do so.
Link-only probes retain the real composition without invoking or flashing it.
The checkpoint still halts; vendor/image input integration and native activation
remain outstanding.

### Explicit microcode compiler inputs

`fstart-ucode` accepts one to three explicitly supplied `--patch HEX_CPUID:PATH`
flat vendor files and `--output PATH`. It checks each exact stepping with the
same driver parser, rejects duplicate equivalent revisions, sorts by CPUID and
concatenates unchanged bytes. Reads are capped at 3201 bytes per input; existing
outputs are never overwritten. Per-input and bundle SHA256/revision reports
record bytes, **not authentication or freshness**. The owner must obtain and
independently trust the vendor inputs; no vendor bytes are checked into fstart.

With platform feature `linked-microcode`, bare-metal compilation requires
`FSTART_AMD_MICROCODE_BUNDLE` to name that explicit immutable file. Its bounded
3200-byte records are compiled into align16 reset-image rodata, not initialized
writable state or flash-XIP references. `microcode::linked::selected_source()`
validates the entire bundle before selecting the exact CPU stepping. Malformed
non-selected entries, duplicate revisions, missing steppings and unsupported
CPU identities fail; there is no best-version selection or alternate-source
fallback. The whole reset image must still fit the production linker window.

`fbuild` accepts repeated `--compiler-input ENV=PATH` selections, forwards them
unchanged through its command adapter, and records them in the host selection.
Invalid names/empty paths and duplicate namespaces fail before host resolution;
every request must match at least one consuming unit's file binding with the
unchanged path. Unknown requests and substituted paths fail before firmware
compilation, including substitutions in other declared units. Platforms still
own supported input namespaces and byte budgets; this is not a flag/env override.

The shared build-plan transport now supports explicit `FileBinding` inputs:
workspace-relative or absolute paths, compiler environment names and byte limits.
The generic executor reads each bounded regular file once, binds canonical origin,
namespace and bytes into the consuming unit directory, and supplies a synchronized
read-only snapshot to the compiler. `compiler-inputs.json` records source, snapshot,
size and SHA256 without embedding payload bytes. Cached symlinks, writable files
or byte corruption are rejected, never overwritten; other units cannot inherit
unbound input environment keys. Content changes invalidate the consuming unit.
The build owner must exclude filesystem mutation through compiler execution;
read-only permissions and hashes are not adversarial isolation or authentication.

This is compiler-input plumbing, not a trusted build pipeline or supported AMD
`fbuild` plan. The caller must preserve source integrity through compilation and
establish physical source decoding/coherence and authenticity/freshness before
staging. Never give the reset-image source address to PATCH_LOADER; the existing
permanent staging path remains mandatory. Neither selector nor tool activates
CPUs. Native boot/OS flow, cache/reset prerequisites and hardware validation
remain outstanding.

### AMD configuration-access instruction contract

### LPC/SPI/SMBus preinitialization

`picasso::preinit` supplies source-backed LPC enable/decode clearing and SPI
BAR setup before cold eSPI connection, and separate ROM-CS/prefetch/SPI100 mode
and clock programming afterward. Validated EFS/board speed overrides lower
other speed classes using hardware encoding order, not numeric enum order.
The byte/word/dword access widths and repeated LPC write are preserved.
SMBus/ASF initialization sets decode, native divider41 and W1C statuses; the
native nominal400-kHz label is not a measured maximum-frequency guarantee.

These are not flash transactions or arbitration. Host ownership against
PSP/AP/SMM/EC and absence of ROM Armor are prerequisites. An active PSP/EC link
must not be destroyed by calling cold LPC preparation. Busy/unavailable SPI
rejects before writes. AOAC now also rejects all-ones control/state rather
than treating inaccessible registers as power/clock readiness, and preflights
all controls before power writes. Any partial programming failure requires
stop/reset.

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

