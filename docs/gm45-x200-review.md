# GM45 / ICH9 / X200 review

Reference: local coreboot checkout at
`59d5a8ea578716ed91a9b442e2fc56e46ce2573d`.
Comparison is source-level, not a report of a successful boot on hardware.
The `gm45-review` jj workspace was initially rebased onto local `master`
(`c395de62`), then onto fetched `origin/master` (`368ff559`). It is now based
on `8ea9901f`, the merge of PR #105. The rebases preserve the upstream
early-init timestamp, board hook, SMBIOS OEM-string field and PCI-bridge
routing representation. GM45 uses the shared `x86::boot` reset entry point;
its runtime PIRQ view borrows route slices, including the new bridge field.
The original workspaces are unchanged, and the 15 chipset commits remain
separate. Additional changes adapt that port to the current framework and
restore the board binding.

## Findings addressed

| Area | Discrepancy and correction | Coreboot reference |
| --- | --- | --- |
| Boot ordering | ICH9 DMI setup was run before RAM training, while GM45 PM ran too late. Now: DDR3 initialization and probes → GM45 PM → ICH9 DMI → GM45 egress/DMI/IOMMU → ICH9 VC1 completion. | `gm45/romstage.c`, `i82801ix/dmi_setup.c` |
| Warm resets | The port lacked the scratchpad `0xcafe` / GEN_PMCON_3 check, interrupted-RAM-init marker, and PMSTS warm/self-refresh check. Added these guards and Cantiga's tiny rank-map/CLKCFG/DCC preparation before CF9. ICH9 prepares PMIR before these checks, in pre-console setup. | `gm45/early_reset.c`, `gm45/raminit.c` |
| S3 rejection | Resume cannot replay this DDR3 training state. Before the common unsupported-resume reset, the northbridge now prepares the controller's reset geometry. This is cold-reset fallback, **not S3 support**. | `gm45/early_reset.c` |
| TXT | Added the TXT-fuse-gated wait for TPM_ACCESS_VALID. Unlike coreboot's unbounded loop, it fails with a timeout after approximately one second. | `gm45/romstage.c` |
| Stage boundaries | A fresh ramstage northbridge had no `dram_ready` state, making memory reporting and TSEG unavailable. Stage-local initialization now validates the live TOM/TOLUD/TOUUD/GGC/ESMRAMC decode without retraining or probing live RAM. | `gm45/northbridge.c` resource readback |
| Interrupts | An assumed X200 PIRQ table is not justified by coreboot, which retains the reset/strap router. Added an exclusive preserve/readback policy; `_PRT` uses actual DxxIR, including PCIe swizzling. PEG/IGD/ME slots outside the router use coreboot's hardwired pin-to-PIRQ identity. The shared routing view can now borrow runtime data. | `intel/common/rcba_pirq.c`, `i82801ix/lpc.c` |
| LPC windows | Reused a common validator/encoder for naturally aligned power-of-two windows; invalid windows fail rather than creating a misleading decode. ICH7/8 use the same helper. | `intel/common/lpc.h` |
| SMM contracts | Updated SMRAM readback verification, SMI quiesce/restore and global locking interfaces. Corrected the obsolete shared mainstage ACPI-mode call. | Current fstart master SMM contracts |
| CPU binding | X200 uses BGA956, not a Socket P package. Named that CPU policy correctly; chipset pairing accepts independent CPU-package policy. X200 selects two CPUs and 64 KiB CAR at `0xfefc0000`. | `socket_BGA956/Kconfig`, X200 Kconfig |
| Console | The rebased bookmark lacked a buildable X200 board package. Restored it against current interfaces and selected a reusable EHCI debug console, not the old dock-UART binding. | X200 `USBDEBUG_HCD_INDEX=2`, `drivers/usb/ehci_debug.c` |
| PCI ownership | Reserved the active EHCI BAR through the existing fixed-BAR allocator contract. ICH9 validates controller enablement and rejects overlapping RCBA/DMI/HPET/APIC/ECAM/flash intervals. | Existing fstart PCI resource ownership |
| RAM preflight | Moved final-map validation ahead of destructive controller programming/training. Unsupported asymmetric geometry now fails before JEDEC, rather than after calibration. | `gm45/raminit.c` memory-map calculations |
| EC defaults | Restored GPIO42 SPD mux, Bluetooth GPIO7 detection, dock-ID GPIO2/3/4 and GPIO28 connection. Use existing H8/PMH7 drivers for EC setup, including radio/TrackPoint/USB/default policy. Added the missing reusable H8 UWB register operation using its typed EC access. | X200 `romstage.c`, `devicetree.cb`, `variants/x200/dock.c`, `ec/lenovo/h8/h8.c` |

### Independent review follow-up

The Opus/GPT source review found additional issues that are now addressed:

* BGA956 selects a model-1067x Penryn driver, not the Merom-only driver.
  It recognizes `10676/10677/1067a`, uses alternative SMRR and performs the
  capability-gated C5/C6, Super-LFM, turbo, EMTTM and thermal MSR sequence.
  CPU and AML consumers share one allocation-free SpeedStep table generator;
  chipset policy supplies C5/C6/SLFM requests instead of hardcoding X200 flags.
* IGD preflight derives the display clock from the selector it will program,
  rather than caching GCFGC's previous value.
* Read/write training retains coreboot's nonfatal upper-edge behavior
  (clamped overflow or the initial narrow-eye bound). Missing lower edges
  and invalid register encodings remain fatal; all searches remain bounded.
* RCOMP cleanup issues one pulse without requiring capture status 4. DMI
  configures VC1 before enabling it and sets clock/exit prerequisites before
  write-once ASPM enable. CLKCFG PM writes retain coreboot's 16-bit sequencing.
  Core counts 1/2/4 are admitted; this does not qualify a single-core X200 SKU.
* ICH9 emits root `_S5` without advertising S3. GM45 emits CPU devices and
  the root `PPKG` consumed by both H8 passive-cooling methods.
* X200 restores the separate PMH7 TrackPoint and touchpad power gates.
* H8 retains the `0xb0` threshold backend for later ThinkPads and adds an
  explicit `0x24/0x25` selector-based backend, selected by X200. Both expose
  `BCCS/BCSS/BCTG/BCSG`. A shared mutex protects selector + data, and the
  `H8_BAT_THRESHOLDS_BIT7` encoding difference is explicit policy.
* EHCI descriptor discovery retries at reserved address 127 after address 0
  fails, and does not reassign an already-addressed gadget.
* The common AML parser preserves literal/interpolation-leading return
  operators and accepts parent-prefixed method-call expressions. Expanded
  feature checks also repaired i945's static PIRQ-policy lifetime.

These are source corrections, not claims of physical validation. SMM/SMRAM
locking is not a claim of BIOS flash write protection: production BLE/SMM_BWP
policy and BIOSWR handling remain separate development work.

## Detailed comparison

### GM45 RAM initialization

Reviewed the port's SPD/timing selection, clock encodings, clock crossings,
RCOMP, DRAM control/timing/bank/ODT programming, DDR3 I/O tables, temporary
rank map, JEDEC command sequence, ZQ, receive-enable calibration, read/write
training, final map, thermal programming and graphics setup against
`raminit.c`, `raminit_rcomp_calibration.c`,
`raminit_receive_enable_calibration.c`, and `raminit_read_write_training.c`.

* Both RCOMP stepping tables (64 × 8 each, 1024 codes total) were mechanically
  expanded and compared to coreboot: identical. The compressed conversion-A1
  exceptions match the reference.
* All 40 read-training pattern words and 80 write-training words match.
* Receive-enable normalization, raw-card-F lane/probe maps, the two edge
  searches, preamble backstep and per-channel minimum-C/group-PRE conversion
  follow coreboot. The Rust version additionally bounds searches and rejects
  field overflow instead of wrapping it.
* DDR3-1067 read/write eye training uses the temporary sequential 128 MiB/rank
  map. It runs only at that rate, as in coreboot; receive-enable calibration
  still runs at lower rates. Training is not replaced by sparse RAM probes.
* The final map already uses a **2048 MiB PCI MMIO hole**, matching X200's
  devicetree and coreboot's default. This was checked, not changed. The UMA,
  GTT/shadow-GTT, 2 MiB TSEG, ME reservation and 64 MiB reclaim arithmetic
  are represented in the existing map tests.
* Contrary to a preliminary suspicion, unequal DIMMs are **rejected**, not
  blindly interleaved. Coreboot implements dual-asymmetric mode; this port
  only admits single-channel or equal-capacity dual-interleaved layouts.

Sparse destructive probes gate publication of RAM after initialization.
They are not exhaustive DRAM qualification, and host arithmetic/MMIO tests
cannot establish that analog training succeeds on an X200.

### ICH9

Reviewed early LPC/SPI/watchdog/GPIO/SMBus work, function disabling, PCIe
pre-disable programming and hiding, DMI/VC1, EHCI/UHCI, SATA AHCI setup,
HDA and ACPI routing against `i82801ix/{early_init,dmi_setup,i82801ix,lpc,
pcie,sata,usb_ehci,azalia}.c` and the common PIRQ generator.

The Intel EHCI FCREG writes are **not an error**: `usb_ehci.c` alone only
sets bus mastering, but `i82801ix.c::i82801ix_ehci_init()` programs exactly
those additional bits. Keep them; do not reset or relocate the active debug
controller in ramstage device initialization.

SATA mode must be selected before BAR allocation; AHCI MMIO programming
must consume the assigned BAR5 afterwards. The current port has this split,
including the mobile port mask, CAP/PI/VSP handling and indexed tuning.
Disabled top-contiguous PCIe ports need CIR programming before FD/RPFN hide
bits prevent access; the port includes that ordering.

### Board

Board facts were compared with X200 Kconfig/devicetree, variant GPIO/dock
sources, HDA verbs and H8 configuration. The board uses shared chipset and
EC mechanisms rather than board-local ECAM/MMIO helper implementations.
The H8 ACPI emitter supplies battery, thermal and ThinkPad hotkey methods;
board glue supplies `_PTS`, `_WAK` and `_L18`.

## Remaining differences and risks

These are not claimed fixed by compiling the board:

1. **No hardware boot result.** DDR3 electrical setup, reset behavior, TXT
   timing, VC negotiation, SMM relocation and debug-gadget enumeration need
   physical validation with recoverable SPI flashing.
2. **RAM support is narrower than coreboot.** No DDR2, SFF, S3 training replay,
   asymmetric dual channels, arbitrary early steppings, or FSB1067/DDR3-667
   special PLL sequence. Unsupported inputs fail closed. Cached cold-boot
   training is not implemented by this board binding.
3. **ACPI/OS parity is incomplete.** Basic CPU P-state and C1/C2/C3 AML is
   present; deeper C-state AML, DMAR, full dock hotplug/disconnect glue,
   Wacom handling, Lenovo EEPROM serial/UUID identity
   and variant-specific display behavior need separate work. H8 brightness
   hooks remain no-ops. No successful OS sleep/resume claim is made.
4. **Northbridge device policy is smaller.** The current GM45 binding does not
   reproduce every coreboot devicetree-driven ME/PEG function/subsystem-ID
   policy or above-4G PCI-resource policy. X200 uses integrated graphics and
   its bundled LED VBT; tablet/CCFL/X301 variants are not qualified.
5. **EC policy is fixed.** Coreboot exposes CMOS options and additional
   presence detection. This binding uses explicit defaults; advertised WWAN
   and UWB capabilities still require testing on the particular board.
6. **EHCI scope is deliberately small.** Only directly attached high-speed
   Appendix-C debug gadgets are supported, not hubs, arbitrary USB serial
   adapters or general USB enumeration. Stages reset/re-enumerate rather
   than preserving console state. Controller-to-external-connector wiring
   remains to be identified on hardware.
7. **Firmware-image layout is specific.** The declared layout is the Lenovo
   6D/8 MiB descriptor with a 2 MiB BIOS region at `0x600000`. It is not the
   7X/4 MiB layout. Vendor descriptor/ME/GbE/PDR bytes are not bundled.
   Verify the target's descriptor and preserve its non-BIOS regions.

## Validation

See the X200 board README for build commands and hardware bring-up steps.
Initial validation performed in the review workspace (before the latest
`origin/master` rebase):

* Release compilation of bootblock, postcar, ramstage and SMM: passed.
* Release halt-image assembly: passed, 8 MiB output. Bootblock is 143360 bytes;
  postcar 24456 bytes; ramstage 116668 bytes compressed (166664 unpacked).
* Intel driver tests: 97 passed for GM45/ICH9; 106 passed with ICH7/ICH8,
  i945 and GM965 additionally enabled.
* USB-debug tests: 6 passed. Intel platform host tests: 11 passed.
* Lenovo driver tests with `fstart-arch/x86_64,acpi`: 5 passed.
* Unoptimized/debug assembly: **failed** the postcar IMAGE budget (64 KiB),
  with the final initialized sections approximately 89 KiB over budget.
  The layout guard also catches overlap with BSS/stack. Use release; do not
  bypass the guard or treat the debug build as a usable image.

After rebasing onto `368ff559`, release compilation and 8 MiB halt-image
assembly passed again: bootblock 147456 bytes, postcar 26736 bytes, ramstage
126695 bytes compressed (183064 unpacked). Re-run host tests passed: Intel
108, USB-debug 6, platform 14, Lenovo 6. Formatting checks passed. Debug
assembly was not re-run; its earlier budget limitation remains unqualified.

After rebasing onto PR #105's merge (`8ea9901f`), release compilation and
8 MiB halt-image assembly passed: bootblock 147456 bytes, postcar 27072
bytes, ramstage 127413 bytes compressed (183528 unpacked). Host tests passed:
Intel 111, USB-debug 6, platform 14, Lenovo 9. Root and board formatting
checks passed. Debug assembly was not re-run, and no hardware was tested.

After the independent review corrections, host verification passed:
architecture 26, combined Intel drivers 121 (+3 doctests), all four platform
families 25 (+2 southbridge DSDT integration tests), USB debug 7,
Lenovo 11 (+1 doctest), AML macros 12, and generated
X200 board facts 1. The common ACPI crate passed 126 tests including doctests.
The combined firmware `stage/mp/acpi` configuration also
passes `cargo check` for `x86_64-unknown-none`. ACPICA resolves `_PSL` to both
CPU devices and executes `_PSS/_PCT/_CST`, `_S5` and the threshold interface.
Release halt assembly passes with existing layout guards: bootblock 147456
bytes, postcar 26808, ramstage 135259 compressed / 194736 unpacked, in the
8 MiB container. Generated host board tests retain the known
`fstart_stage_env` check-cfg warnings. No hardware was tested.

Assembly required populating the two microcode files from the already-present
local checkout. The generated keys are ignored local development keys, not
production signing material.

Host coverage includes map/timing/training arithmetic, typed register layout,
reset geometry, interrupt policy and fixed-BAR validation, USB setup/packet
packing, capability-cycle rejection, offline behavior, lock contention and
stuck-engine timeout. The follow-up also covers reserved-address recovery at
its control-request seam, Penryn matching/capability arithmetic and shared
SpeedStep/EMTTM tables. ACPICA tests execute both threshold layouts (including
both bit-7 policies), soft-off and the combined thermal/processor namespace.
Simulated EC RAM checks selectors/encoding, not real EC firmware storage.
None of these tests qualifies physical USB enumeration or DRAM training.

LSP probes in the separate workspace returned inconclusive results; compiler
and firmware-build results are the validation authority, not the absence of
LSP messages.
