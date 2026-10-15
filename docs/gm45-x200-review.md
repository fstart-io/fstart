# GM45 / ICH9 / X200 review

Reference: local coreboot checkout at
`59d5a8ea578716ed91a9b442e2fc56e46ce2573d`.
Comparison is source-level, not a report of a successful boot on hardware.
The `gm45-review` jj workspace was initially rebased onto local `master`
(`c395de62`), then onto fetched `origin/master` (`368ff559`) and `8ea9901f`
(the merge of PR #105). The PR #40 update is based on fetched `origin/master`
`521e4aee` (PR #121): a copied stack preserves the prior review and chipset
history. The rebases preserve the upstream early-init timestamp, board hook,
SMBIOS OEM-string field and PCI-bridge
routing representation. GM45 uses the shared `x86::boot` reset entry point;
its runtime PIRQ view borrows route slices, including the new bridge field.
The original workspaces are unchanged, and the 15 chipset commits remain
separate. Additional changes adapt that port to the current framework and
restore the board binding. The latest rebase retains the newer early-hook
ordering and typed ICH7 GPI policy, while adapting the new desktop CPU package
and Pineview static PIRQ policy to this branch's shared contracts.

## Findings addressed

| Area | Discrepancy and correction | Coreboot reference |
| --- | --- | --- |
| Boot ordering | ICH9 DMI setup was run before RAM training, while GM45 PM ran too late. Now: DDR3 initialization and probes → GM45 PM → ICH9 DMI → GM45 egress/DMI/IOMMU → ICH9 VC1 completion. | `gm45/romstage.c`, `i82801ix/dmi_setup.c` |
| Warm resets | The port lacked the scratchpad `0xcafe` / GEN_PMCON_3 check, interrupted-RAM-init marker, and PMSTS warm/self-refresh check. Added these guards and Cantiga's tiny rank-map/CLKCFG/DCC preparation before CF9. ICH9 prepares PMIR before these checks, in pre-console setup. | `gm45/early_reset.c`, `gm45/raminit.c` |
| S3 rejection | Resume cannot replay this DDR3 training state. Before the common unsupported-resume reset, the northbridge now prepares the controller's reset geometry. This is cold-reset fallback, **not S3 support**. | `gm45/early_reset.c` |
| TXT | Added the TXT-fuse-gated wait for TPM_ACCESS_VALID. Unlike coreboot's unbounded loop, it fails with a timeout after approximately one second. | `gm45/romstage.c` |
| Stage boundaries | A fresh ramstage northbridge had no `dram_ready` state, making memory reporting and TSEG unavailable. The mainstage's `early_init` now validates the live TOM/TOLUD/TOUUD/GGC/ESMRAMC decode when the raminit-complete marker is present, without retraining or probing live RAM; soft-reset recovery runs only in the bootblock's `dram_init`. | `gm45/northbridge.c` resource readback, `gm45/romstage.c` |
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

A second discrepancy pass addressed the remaining supported-path findings:

* X200 declares its own 100°C critical / 90°C passive thermal trips, matching
  coreboot's GNVS policy (initial active-cooling trip 80°C), rather than using
  the H8 fallback temperatures.
* SMBIOS uses the non-OEM `CBET4000 fstart …` firmware ID and `ThinkPad X200`
  product-version string required by Linux `thinkpad_acpi` identification.
  The board reads the real EC OEM string through the existing H8 helper;
  unavailable EC data is logged and omitted, not fabricated. EEPROM serial
  and UUID provisioning remain separate work.
* The shared ICH SMM handler acknowledges only observed legacy PM1 status and
  enters S5 on a power-button event. With SCI enabled it leaves PM1 status
  for OSPM, including on an unrelated SMI.
* H8 emits Fn-F2/Fn-F3 EC query methods, honoring both default and alternative
  layouts. Its existing extended-battery capability now emits `_BIX` for both
  batteries and reads cycle count from management-page offset `0xa4` under
  the same EC mutex. `_BIF` and both threshold backends remain available.

These are source corrections, not claims of physical validation. SMM/SMRAM
locking is not a claim of BIOS flash write protection: production BLE/SMM_BWP
policy and BIOSWR handling remain separate development work.

### Third discrepancy pass

A further comparison against coreboot `65d90e75` (no GM45/ICH9/X200, model
1067x, SMM or H8 changes since the reference) found and corrected:

* **Mainstage reset loop.** The mainstage reruns northbridge `early_init`
  after the bootblock wrote the `0xcafe` raminit marker and acknowledged
  GEN_RST_STS, which is exactly the soft-reset condition: every boot reset
  before the bus scan. Recovery and the TXT wait moved into `dram_init`
  (coreboot romstage only). The same fresh mainstage northbridge also failed
  `detect_memory` before the late `stage_local_init`; `early_init` now
  recovers the trained decode when the marker is present.
* **Host bridge ACPI.** `PCI0` gains coreboot's `_CRS` (bus range, I/O, VGA,
  option ROM, TOLUD..0xfebfffff, above-TOUUD and TPM windows), the
  `i82801ix` `_OSC` handing PCIe features to the OS, and the complete `PDRC`
  reservation including RCBA and the 0xfed2xxxx/0xfed4xxxx ranges.
* **Graphics.** GM45 now runs `gma_func0_init()`: bus master, a cleared 2 MiB
  GTT, devicetree panel power sequencing, and `BLC_PWM_CTL` from the panel's
  EDID string through the board's `blc.c` table. The OpRegion is published
  with the LED or CCFL VBT chosen as `mainboard_vbt_filename()` does, the
  shared GMA library performs the native modeset on DP2/HDMI2/VGA/LVDS, and
  the shared `GFX0` node (moved out of the GM965 driver) carries brightness;
  H8 Fn brightness calls `GFX0.INCB/DECB` as in the X200 DSDT.
* **Subsystem IDs.** Board devicetree subsystem IDs are programmed before
  device init (type-0 headers, bridges through SSVID). Linux applies its X200
  CX20561 pin fixup by HDA 17aa:20f2.
* **ExpressCard.** Root port 4 is enabled; ports 1-4 are 10 W slots, port 4
  hot-pluggable, per `variants/x200/overridetree.cb`.
* **Board policy.** `power_on_after_fail` defaults off (`cmos.default`),
  PMH7 dock events are enabled, LPT and both game-port decodes are on like
  coreboot's `LPC_EN`, `_CST` uses the X200 table (C3 latency 55 us, C6 on
  battery through `\PWRS`), and `\PNOT` notifies the CPUs through `\CNOT`.
  The root `\UCMS` method lets `thinkpad_acpi` drive the ThinkLight.
* **ICH9.** `PS2K`/`PS2M` are emitted (shared with ICH8). CF9GR is no longer
  set: coreboot's bit-20 PMIR write is truncated to a byte, so CF9 resets stay
  host resets. The TCO watchdog is acknowledged early and reloaded in the
  mainstage instead of halted. `INTERRUPT_LINE` follows each function's
  PIRQA-D route like `i82801ix_pirq_init()`; NMI masking reads port 0x74.
* **FADT.** `TMR_VAL_EXT` is no longer advertised: ICH and QEMU PM timers are
  24 bits. ICH9 reports PM2_CNT, a 57 us C3 latency, a 3-bit throttle duty
  field and docking support; laptops use the mobile PM profile.

### Fourth pass: remaining coreboot parity

The gaps the third pass left documented are now ported:

* **ME functions.** A new mainstage `chip_init` hook (coreboot
  `gm45_init()`, before enumeration) hides the devicetree-disabled ME
  functions 03.1-03.3 in DEVEN, after raminit sized the ME memory from 03.0.
* **MCFG and DMAR.** GM45 had no MCFG at all, so the OS saw no ECAM window.
  It now emits MCFG and coreboot `gm45/acpi.c`'s DMAR: the HDA unit, the
  graphics unit unless B2 stepping or IGD absent, the ME unit when 03.0 is
  enabled and the catch-all VC0 unit, at the bases `init_iommu()` programs.
  A generic DMAR builder lives in `fstart-acpi`.
* **`_DOD`.** The shared `GFX0` node reports coreboot's
  `GMA_STATIC_DISPLAYS` (VGA0, TV0, LCD0 at 0x410) with the stub
  `_DCS/_DGS/_DSS`, for GM965 and GM45 alike.
* **Dock.** `acpi/dock.asl`: `\_SB.DOCK` with `_DCK/_STA`, the GPIO2-4 dock
  ID (`GGID`), `\GP28` and EC `DKR1`, and EC queries `_Q18/_Q37/_Q45/_Q50/
  _Q58/_Q5A`. Only the GPIO level bits the methods use are declared.
* **EEPROM identity.** As on the X61, the AT24RF08C part number, version,
  serial and UUID describe system and baseboard in SMBIOS; the static
  identity is the fallback when the EEPROM cannot be read.
* **X200t digitizer.** The PC87382 is programmed as the overridetree does
  (LDN 3 at 0x200/IRQ 5 with 0x29/0xf0, LDN 2/7/0x19 off). As in
  `drivers/lenovo/wacom.c`, a responding SuperIO plus a tablet machine type
  adds the `WACF004` `DTR_` device; X200 has no dock serial port.
* The X200 DSDT glue moved into `fstart-driver-lenovo::x200`, next to the
  X6 family glue, so ACPICA executes the dock, `\UCMS` and digitizer paths.

### CodeRabbit follow-up

* DDR3 addressing rejects reserved row encodings (only 12–16 row bits and
  9–12 column bits are valid), matching `device/dram/ddr3.c`. A test covers
  every row/column encoding.
* ICH7 and ICH9 share the existing GPI destination enum/encoder and legacy
  PIRQ validator. GM45 checks PIRQ bytes during const construction; ICH9
  also checks direct runtime configs. X200's GPIO1/GPIO8 SCI routes and IRQ
  assignments are unchanged.
* The GM45 ACPICA test uses an automatically cleaned temporary directory and
  skips only a missing executable. Other launch errors and namespace failures
  still fail. The EHCI timeout test injects its delay rather than discovering
  clocks through potentially privileged host MSR reads; firmware still uses
  the architecture delay.
* Selector 0 retains Coreboot's "any battery" threshold policy: either
  successful readback is sufficient. Selectors 1/2 verify that battery.
  Changing this to an unconditional OR would reject an absent second battery;
  both threshold backends remain unchanged.
* CPU AML helpers are feature-gated to their consumers, including the
  Pineview-only case, fixing the workspace CI dead-code failure without
  suppressing warnings.

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
* As in coreboot `collect_dimm_config()`, both populated channels always use
  dual-interleaved mode, including unequal capacities: each channel's rank
  bounds start at zero and an active ME reserves 32 MiB instead of 64 MiB.
  Coreboot's "dual asynchronous" mode is only the temporary pre-JEDEC map.

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
   arbitrary early steppings, or FSB1067/DDR3-667 special PLL sequence. Unsupported inputs fail closed. Cached cold-boot
   training is not implemented by this board binding.
3. **No S3.** No successful OS sleep/resume claim is made; resume requests
   fall back to a prepared cold reset.
4. **Northbridge device policy is smaller.** The firmware allocator assigns no
   BARs above 4 GiB (the window is only advertised to the OS). The X200t
   digitizer and the X301 variant are not qualified on hardware.
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

After the second discrepancy follow-up, Lenovo passed 12 unit tests and one
doctest, combined Intel drivers 122 unit tests and three doctests, all four
platform families 25 unit tests and two DSDT integration tests, and generated
X200 board facts one test. The combined firmware `stage/mp/acpi` target check
and root/board formatting checks passed. ACPICA executes both hotkey layouts,
both extended-battery capability settings, each battery's `_BIF` and optional
`_BIX` (including cycle count), and the X200 100/90/80°C thermal trips.
The PM1 ownership test covers legacy versus SCI ownership and unrelated SMI
sources. SMM disassembly confirms the source/SCI/status gates and inlined S5
write; its ELF has no unresolved symbols, calls or relocations. This is not
an executed power-button or Linux-driver probe test on hardware.

Release halt assembly again passed without changing layout guards: bootblock
147456 bytes, postcar 27144, ramstage 136272 compressed / 196640 unpacked,
in the 8 MiB container. Generated host tests retain the known check-cfg warnings.
No physical EC/USB/thermal/OS/SMM qualification occurred.

The copied PR #40 stack on `521e4aee` passed architecture 30, Intel drivers
127 (+3 doctests), Lenovo 12 (+1 doctest), USB debug seven, Intel platforms
26 (+2 DSDT integrations), and generated X200 board facts one. The combined
firmware `stage/mp/acpi` check and formatting checks passed. Release halt
assembly remained within the unchanged guards: bootblock 147456 bytes,
postcar 27160, ramstage 136283 compressed / 196640 unpacked, 8 MiB container.

The CodeRabbit follow-up passed `cargo test --locked --workspace` with Cargo
warnings denied, Intel drivers 129 (+3 doctests), Intel platforms 27 (+2 DSDT
integrations), the combined firmware target check, generated X200 board facts
and formatting. The ACPICA harness was also exercised with a missing tool,
an injected namespace failure and a non-executable tool: each retained the
expected skip/failure result and removed its temporary directory. ACPI-only
and Pineview-only feature checks passed; the former retains a pre-existing
unused-dependency manifest warning. Release halt assembly passed the unchanged
guards: bootblock 147456 bytes, postcar 27160, ramstage 136314 compressed /
196704 unpacked, 8 MiB container. Hardware qualification remains outstanding.

The third discrepancy pass passed `cargo test --workspace` with warnings
denied, Intel drivers 121 with GM45/ICH9, GM965/ICH8, i945/ICH7, ACPI and
display enabled (+3 doctests), the GM45/ICH9 set 107 including ACPICA
execution of `PCI0._CRS/_OSC`, `PDRC`, `PS2K`, `GFX0.LCD0._BCL`, the battery
`_CST` and `\PNOT`, Lenovo 12 (+1 doctest), common ACPI 104 (all features),
the PCI subsystem-ID test, Intel platforms 11 (+2 DSDT integrations), host
warning-denied feature checks (ACPI-only, Pineview, GM45 with and without
ACPI, GM965 with display, i945, ICH8/ICH9) and `cargo fbuild check
lenovo-x200`. X61, X60, D945GCLF and QEMU Q35 release builds still link.
Release halt assembly passed the unchanged guards: bootblock 147456 bytes,
postcar 27160, ramstage 180567 compressed / 251912 unpacked (the GMA library),
8 MiB container. Native graphics, GMBUS EDID and the backlight are untested
on hardware.

The fourth pass passed `cargo test --workspace` with warnings denied, Intel
drivers 121 (+3 doctests) with every legacy chipset, GM45/ICH9 107 (ACPICA now
also runs `GFX0._DOD`), Lenovo 14 (+1 doctest, ACPICA runs `GGID`, `_DCK`,
`_STA`, `_Q50`, `_Q5A`, `\UCMS` and the digitizer `_CRS`), common ACPI 105
including the DMAR wire-format test, the warning-denied feature checks and
`cargo fbuild check lenovo-x200`. X61, X60, D945GCLF and Q35 still build.
Release halt assembly passed the unchanged guards: bootblock 147456 bytes,
postcar 27160, ramstage 186918 compressed / 260816 unpacked, 8 MiB container.

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
