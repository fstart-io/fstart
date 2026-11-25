# Haswell / Lynx Point

This is the platform binding for [native Haswell DDR3 initialization](haswell-raminit.md),
pairing the Haswell System Agent (`driver-intel/src/haswell/`) with the Lynx Point
PCH (`driver-intel/src/lynxpoint/`) through `platform-intel/src/haswell.rs`.
Board-specific EC policy, memory-down SPDs, panel wiring, and VBT live in
board crates, not in the chipset. Assembled images do not supply a machine's
Intel flash descriptor or ME region. The local coreboot reference is
`1958b5d923a`. A linked firmware image is **not** evidence that DRAM training,
SMM, display, or resume works on hardware.

## Source map

Paths in the middle column are relative to `crates/`.

| coreboot | fstart | Responsibility |
|---|---|---|
| `cpu/intel/car/non-evict`, `cpu/intel/haswell/{bootblock,haswell_init,romstage}` | `arch/src/x86/{boot/mod.rs,cpu/intel/haswell.rs,cpu/intel/haswell_vr.rs}` | NEM setup/teardown dispatch, flex ratio, MP matching, microcode, MCA, EIST/turbo, TCC, package/DDR power limits, VR and C-state latencies |
| `cpu/intel/haswell/{smmrelocate,smm_finalize}` | `arch/src/x86/cpu/intel/{smm,haswell}.rs`, `smm/src/save_state.rs` | Serialized EM64T101 relocation; full-TSEG SMRR, separate 4 MiB IED, conditional core/uncore PRMRR, second microcode load, permanent-handler code-access check |
| `northbridge/intel/haswell/{bootblock,early_init,romstage,memmap,northbridge}` | `driver-intel/src/haswell/system_agent.rs` | PCIEXBAR, PAM, fixed SA/VT-d `MmioWindow` BARs, training and inventory, memory map readback (`BDSM`/`BGSM`/`TOLUD`), bootstrap-range checks, E820, WB ranges, SMRAM and lockdown |
| `early_dmi`, DMI path of `early_pcie`, `early_peg`, `vcu_mailbox`, `pcie`, `pcode_mailbox` | `driver-intel/src/haswell/{links,peg,system_agent}.rs` | Traditional external DMI AFE, VCU access, Gen2 retrain, both VC peers, egress topology (`DmiBar`/`EpBar`), PEG slot capabilities; on-package ULT PCODE calibration and PMSYNC |
| `gma`, `minihd`, native display | `driver-intel/src/haswell/igd.rs`, `driver-intel/src/gma/haswell.rs`, `driver-intel/src/igd.rs` | GT force-wake, power well, panel delays and PWM, CDCLK, VBT/OpRegion 2.0, display-audio verbs, and optional native `gma::Haswell` scanout/framebuffer bring-up |
| `cpu/intel/haswell/acpi`, northbridge `acpi.c`, `acpi/*.asl` | `driver-intel/src/haswell/acpi.rs`, `driver-intel/src/cpu/ratio_aml.rs`, `acpi/src/dmar.rs` | PCI0, MCFG, CAPID0_A-gated VT-d DRHD with IOAPIC/HPET scopes and IGD RMRR, processor P/C/T objects |
| `southbridge/intel/lynxpoint/{bootblock,early_pch,early_pch_native,thermal}` | `driver-intel/src/lynxpoint/mod.rs` | LPC decoding, watchdog, IOAPIC, SMBus, native PCH sequencer and thermal sensor setup |
| `lp_gpio`, `intel/common/gpio` | `driver-intel/src/lynxpoint/gpio.rs`, `driver-intel/src/southbridge/gpio_ich.rs` | LP per-pad mux/sense, ownership, wake and sparse PIRQ map; traditional GPIO banks |
| `hsio/`, `iobp`, `early_usb_native`, `usb_{ehci,xhci}` | `driver-intel/src/lynxpoint/{hsio,hsio_tables,usb,smm}.rs` | Bx LP and Cx H PHY tables, lane ownership, bounded IOBP transactions, USB PHY/OC settings, routing and sleep preparation |
| `lpc`, `pch`, `sata`, `serialio`, `azalia`, `pcie` | `driver-intel/src/lynxpoint/{devices,power,pcie}.rs` | Function disables, power optimizer, clock gating, AHCI/Gen3 trim, Serial IO PCI/ACPI mode, PCH HDA, PCIe lane selection and RPFN |
| `early_me`, `me` (ME9) | `driver-intel/src/lynxpoint/me.rs`, `driver-intel/src/southbridge/mei.rs` | ME UMA request, CPU replacement/reset, DRAM Init Done/status, BIOS payload consumption, optional ICC clock disable and End Of Post |
| `fadt`, PCH `acpi/*`, `smi`, `smihandler`, `intel/common/finalize` | `driver-intel/src/lynxpoint/{acpi,smm,mod}.rs`, `driver-intel/src/southbridge/{smi,spi_menu,spi}.rs` | LPC/PCI namespace, interrupt routes, Serial IO resources, GPE geometry, board SMI dispatch, flash journal and lockdown |
| board/platform stage composition | `platform-intel/src/haswell.rs`, `platform-intel/src/acpi_bars.rs` | `HaswellUlt` and `HaswellTrad`, CAR/stack sizes, cache buffers, CPU and driver bindings (`HaswellPch` -> Lynx Point + ME9), S3 ACPI-BAR retention |

## Architecture boundaries

* **Northbridge/southbridge decoupling:** `driver-intel/src/haswell/` has no
  direct imports from `driver-intel/src/lynxpoint/`. Southbridge resources
  (`rcba_base`, `pm_base`, `gpio_base`) and ME9 hooks (`me_uma_mib`,
  `me_cpu_replaced`, `me_dram_init_done`) are supplied through `HaswellPch`
  and `PchResources`, which `platform-intel/src/haswell.rs` binds to Lynx Point
  during `Config::build()`.
* **Typed MMIO windows:** All System Agent MMIO uses bounded `MmioWindow`
  wrappers (`MchBar`, `DmiBar`, `EpBar`, 4 MiB GTTMMADR, 16 KiB Mini-HD,
  VT-d BARs, and `southbridge::rcba_window`), with no raw pointer casts or
  `read_volatile`/`write_volatile` helpers.
* **Persistent storage:** Training cache records and retained S3 stage/BAR
  handoffs use `fstart-store` (not CBMEM), sharing the common Intel bootblock,
  postcar, and ramstage pipeline.

## Boot flow and persistence

1. The shared reset entry loads microcode and selects the IMC NEM path on models
   `3c`, `45`, `46`. Native CAR is `0xff7c0000..0xff800000`, with a 128 KiB
   bootblock stack. The platform microcode file set is `06-45-01` (ULT) or
   `06-3c-03` and `06-46-01` (traditional/Crystal Well). PCIEXBAR decodes 64
   buses from `0xf0000000`. Before console setup the PCH opens LPC/EC I/O
   decodes, stops the watchdog, completes the flex-ratio reset handshake and
   programs GPIO.
2. On the BSP, program the SA BARs/PAM and discover cold/warm/S3. Sample ME
   and PMCON before clearing DISB. Configure the board interrupt map,
   PCH PHYs, thermal and USB. PCH-H also negotiates the external DMI link.
   Run the board's optional SPD selector **after the pads are configured**.
3. Use the static `Workspace::EMPTY` and platform-owned cached-input/capture
   slices. The journal key uses `Board::cache_key` with static addressing,
   board/build and southbridge identity. The inner frame additionally checks
   the actual selected population, wiring, reservations and SPD bytes. Never
   promote an invalid S3 replay into destructive training. Clear DISB and
   reset on failed resume. Send ME Init Done with status 3 when raminit
   reports `Initialized::memory_lost`: warm replay resets the DIMMs even
   without a cold downgrade, and reinitializes ECC parity if applicable.
   Only S3 reuses retained handoff data. Publish successful non-S3 captures
   and copy inventory to the `fstart-store` DRAM store while CAR is still live.
4. Check the linked bootstrap windows against usable low memory, **not**
   TOLUD. TSEG is `[TSEGMB, BGSM)`; subtract the configured DPR size even when
   enforcement (EPM) is off before exposing allocatable RAM. The postcar loader
   uses the existing Intel uncached DRAM transition. Ramstage rebuilds the map
   from hardware (`BDSM`, `BGSM`, `TOLUD`, `TOUUD`), installs RAM MTRRs,
   commits the journal, allocates PCI and initializes the chipset. Both PCH
   variants have the RCBA+`0x3800` SPI engine. The existing flash geometry,
   descriptor-region, extent and write-protection checks still apply.
5. Install MP/SMM, keeping the IED at the top of TSEG out of the image
   allocator. Program SMRR across **all** of TSEG. On cold boot publish the
   verified VBT/OpRegion 2.0, run optional native `gma::Haswell` display
   bring-up, publish ACPI/SMBIOS and finish chipset lockdown/ME EOP. Resume
   keeps the original OS tables and restores ASLS instead of rebuilding a
   framebuffer. `_S3_` is exposed only when the common flow has both a
   persistent training record and retained postcar/ramstage data; it is not a
   hardware certification. A failed retained-stage load also goes through
   chipset reset handling in postcar, even though that stage has no long-lived
   chipset objects.

## Board interface

Use `fstart_platform_intel::haswell` (`hsw` below). Implement `IntelBoardFacts`
with `type Platform = hsw::HaswellUlt` and `CONFIG: &'static hsw::Config<true>`.
Start with `hsw::Config::new(hsw::Gpios::Lp(&PADS))`, set its fields in a
const/static initializer, and call `.build()` to derive the shared LPC windows
and bind Lynx Point resources into `northbridge.pch`.
For PCH-H use `HaswellTrad`, `Config<false>` and `Gpios::Traditional(&GpioConfig)`.
The top-level population fields are `max_cpus` and `subsystem_ids`. Supply the
real board's `IntelIfdFlashLayout`, verified data assets and `.with_memory_cache()`
in `BoardFacts`. Do not substitute a development flash layout for a machine's
actual descriptor.

| Board input | API / units |
|---|---|
| GPIO wiring | `southbridge.gpio`: LP `PadTable = [Pad; 95]` indexed by **physical** GPIO, no sentinel. `Pad::new(Mode::{Native,Unused,Input,Output(bool),Sci,Smi,IrqEdge,IrqLevel,Pirq})`, plus `invert`, `pull`, `blink`, `rsmrst`. `LpGpio.get(pin)` and `.set(pin, high)` use CONFIG0, not the traditional level banks. |
| DRAM | `northbridge.memory.{dq_pins_interleaved,double_refresh,rcomp}`; `tseg_mib`, `dpr_mib`, `igd_enabled`. ME size and boot mode are hardware results, not board constants. `northbridge.slots` is `[[Slot<'static>; 2]; 2]` in channel/slot order. `Slot::SmBus` takes **7-bit** addresses; `Slot::MemoryDown` takes `&'static [u8; 256]`. |
| Dynamic SPD selector | Set `northbridge.select_spd = Some(select)` where `fn select() -> Result<hsw::Slots, fstart_core::services::ServiceError>`. Runs after GPIO/EC setup in the bootblock. Resolve memory-down SPD from the board's trusted pre-memory bytes, not an unverified file load. |
| EC/LPC and IRQs | `southbridge.lpc_generic_io: &[LpcGenericIoDecode]`. `ec_present` enables thermal reporting. `irq: &[IrqRoute { device, pins: Option<u32>, pirqs: Option<u16> }]` holds DxxIP/DxxIR nibbles. `gpe0_en: [u32; 4]`, `alt_gp_smi_en`. PCH-H has `gpi_routing: [GpiRoute; 16]`. |
| USB | `southbridge.usb2: &[Usb2Port { length, enabled, oc_pin, location }]` and `usb3: &[Usb3Port { enabled, oc_pin }]` in physical order. `length` is coreboot's encoded trace length (e.g. `0x0150`), `None` means OC skip. `Location::{BackPanel,FrontPanel,Dock,MiniPcie,Flex,Internal,Skip}`. `xhci_default`, `usb_xhci_on_resume` are independent policies. LP has 8 HS and 4 SS ports. |
| SATA, clocks, PCIe | `sata_port_map`, `sata_gen3_dtle`, `sata_hotplug_map`, `sata_devslp_mux`, `sata_devslp_disable`, `sata_gen3_tx`, `icc_clock_disable`, `pcie_ports`, `pcie_force_aspm`. All masks are indexed by physical port minus one, before coalescing. For traditional PEG, `northbridge.peg: [Option<Peg>; 3]` describes onboard vs slot, slot number and power limit value/scale. |
| Serial IO / other functions | `serialio_enable` bits 0–7: DMA, I2C0, I2C1, GSPI0, GSPI1, UART0, UART1, SDIO. `sio_acpi_mode`, `sio_i2c_1v8`. `adsp`, `gbe`, `mei_aux` select the other optional functions. |
| HDA and display | `southbridge.hda: Option<&'static HdaConfig>`; shared `HdaVerbTable` supports `pre_verbs`, subsystem ID (`AZALIA_SUBVENDOR`), typed pins, and `extra_verb`. `northbridge.minihd` selects the separate CPU display codec. `northbridge.igd` (`HaswellIgdConfig`) configures `vbt: Option<VbtSource>`, `display: Option<IgdDisplayPolicy>` (for native `gma::Haswell` scanout), `brightness_levels`, and `panel` (`hotplug` for B/C/D, `delays: Option<PanelPowerDelays>` in microseconds, `backlight_pwm_hz`, `backlight_active_low`, `ddi_e_connected`). |
| CPU | `northbridge.tcc_offset`, `vr.{min_vid,slow_ramp}`; leave at package defaults unless the board specifies otherwise. |
| Stages and ACPI | Use normal `Bootblock`, `postcar::run` and `Ramstage` phases. EC/SuperIO pin policy is ordinary board code, not a chipset hook table. Pass the board `AcpiDevice<Config=()>` to `emit_tables`; use `PcNamespace::LPC`, `PcNamespace::IGD`, and board AML under the Serial IO controllers. For SMM bind a handler taking `&lynxpoint::smm::Config` and call its `unsafe dispatch(|pending| { ... })` from the rendezvous owner. `pending.apm`, `.sleep`, `.gpi` expose board events. The closure returns `Result<(), PrepareError>`: SCI_EN and sleep commit only after board preparation succeeds. `pending.acpi_enabled()` reports the current owner; `pending.request_poweroff()` requests S5 without a second PM controller. Without board work bind `lynxpoint::smm::handle`. |

## Differences and bring-up limits

* This is the **native** DDR3/DDR3L flow, not a binary `pei_data` ABI.
  No MRC scratch BAR, magic SPD address, `tx_byte`, CBFS pointer, arbitrary
  `max_ddr3_freq`, LPDDR3, Broadwell, engineering A0, OC undervolting or vboot.
  Microcode uses the fstart reset-entry loader; this does not provision an
  Intel FIT/Boot Guard authenticated startup module for fused machines.
  Diagnostics report the native steps rather than replicating coreboot's
  strings, MBP item dumps and event log. PHY recipes are for LPT-LP B0–B2
  and LPT-H C0 or later, not Wildcat Point.
* As in coreboot, an absent, unready or `FPT_BAD` ME is reported and the
  boot continues without ME UMA (and without a DRAM Init Done message).
  A missing DID acknowledgement or an S3/S4/S5 action is logged; only the
  requested resets are performed.
* `Haswell::post_verify_init` always configures GT, power well, panel delays,
  PWM, CDCLK, display audio, and OpRegion 2.0 (when a VBT is configured).
  Native boot-display scanout via `gma::Haswell` runs when `northbridge.igd.display`
  is `Some(policy)` and exposes its framebuffer through `framebuffer_info()`.
  Leaving `display: None` hands scanout to the payload/OS. There is no VBIOS
  fallback.
* SMM relocation uses the shared **serial**, in-memory save-state path, not
  parallel MSR save states. The handler uses the same GPE/SMI decoder as the
  other ICH/PCH boards with the LP or split-H register geometry. USB sleep
  code uses installation-time BARs and skips moved controllers; it does not
  trust an OS BAR as an arbitrary SMM pointer. S0ix/S1 are not advertised by
  default.
* Serial IO AML describes the firmware-assigned BARs directly instead of
  storing them in coreboot GNVS. I2C0/1, GSPI1 and UART0 expose their fixed
  transmit/receive DMA request lines only when the DMA controller is present.
  I2C children, EC masks, keyboard/lid, thermal policy, TPM and SPD binaries
  are board responsibilities. This binding does not provide an early Serial
  IO UART console or EHCI debug port; use the board console or post-CAR flash
  log for bring-up. No generic binary-MRC PEG hide/unhide workaround.
* Locking the SPI opcode menu does **not** enforce BIOS write protection.
  BLE/SMM_BWP, flash descriptor permissions and WP# are separate policy.
  A TCO BIOSWR SMI revokes BIOSWE before acknowledging its status.
  The training store needs a supported chip and a writable, reserved BIOS
  extent. The build does not contain vendor FD/ME/GbE contents or a production
  trust anchor. S3 must not be advertised if persistence fails.
* On resume, compare the requested physical port mask before changing any
  root ports. Check the resulting function map and Serial IO BARs against
  the cold store record (`platform-intel/src/acpi_bars.rs`) before hiding
  Serial IO. A mismatch forces a cold reset.
* Board AML can supply `\_TZ.THRT` and `\_TZ.THRM`, call `MCHC.CTLE(watts)` /
  `CTLD()` and `\PPKG()`, use `LPCB.GPIO.GWAK(pin)` in I²C child `_DSW`, and
  attach USB connector properties under `XHCI.HUB7.PRT*` / `SSP*` using
  `GPLD(visible)`.
* The host tests cover LP pad/IRQ banks, ownership fuses, USB trace/OC
  encoding, memory bounds, PEG/CPU power limits, the EM64T101 IED field,
  DMAR layout, VT-d PDRC gating, `acpi_bars` S3 snapshot matching, and
  traditional and populated LP DSDTs. The fixtures recompile the `iasl`
  disassembly and execute wake and thermal methods with `acpiexec`.
* Hardware bring-up is still required on real Haswell/Lynx Point boards:
  verify GPIO/EC SPD selection and `GEN_PMCON_2`, ME UMA/DID before CAR
  teardown, replay/cache rejection on cold/warm/S3, final E820/SMRR, all AP
  SMI returns, Serial IO devices, SATA, USB routing/OC, audio, native
  `gma::Haswell` display bring-up, wake sources, and repeated suspend cycles.
  PCH-H PEG/DMI and ECC need separate machines.

## Reproducing the checks

With ACPICA (`iasl` and `acpiexec`) and the Rust targets available:

```sh
cargo test -p fstart-driver-intel -p fstart-platform-intel
cargo clippy -p fstart-driver-intel -p fstart-platform-intel --all-targets -- -D warnings
cargo fmt --all -- --check
cargo ffmt --check
cargo fbuild checks
```

## CPU / system agent

* **Steppings:** Both the native IMC and the System Agent accept Haswell B0
  (`0x306c2`, `0x40650`, `0x40660`) and C0 (`0x306c3`, `0x40651`, `0x40661`)
  steppings. Note that the default platform microcode asset list pins C0
  microcode (`06-45-01` for ULT; `06-3c-03` and `06-46-01` for traditional /
  Crystal Well), so B0 engineering samples require matching microcode assets
  if microcode patching is desired.
* Haswell patches all CPUs before relocation. The second load is best effort:
  it may reapply an equal revision, never a lower one. Missing microcode is not
  an SMRR failure. Every CPU must relocate and install the required SMRR before
  permanent SMIs are enabled. Unlike coreboot `post_mp_init`, fstart closes and
  locks SMRAM **before** enabling permanent sources; no event may dispatch
  through a writable permanent handler. Per-CPU setup and the BSP maximum
  ratio complete while sources are quiesced. Coreboot's `lpc_final()` sends
  APM FINALIZE on S3 or for configured chipset lockdown. fstart instead checks
  the package fetch lock once, at the first permanent-handler dispatch (at
  latest the mandatory smoke SMI), without depending on an APM command.
  The write-once lock is verified; POST `e7` and halt means the CPU
  advertised the protection but did not enable and lock it.
* `\PNOT()` fans out `0x81` through `\_SB.CNOT`; `\PPCN()` and `\TNOT()`
  use `0x80` and `0x82`. `\TLVL` is the mutable package throttle limit. Haswell
  P-state domains contain one logical CPU each; T-state coordination is shared.
* `CTLE(watts)` / `CTLD()` manage dynamic package PL1 clamping rather than
  configurable-TDP level selection. `STND`, `STDN`, selection of fused CTDP
  down/up levels and their PL2 transitions (fixed 25 W on ULT, 125% on
  traditional) are unsupported. `CTLE` uses `watts << PUNI` including exponent
  zero, rejects overflow, and preserves the original PL1, clamp and TAR across
  repeated requests. Package limits at boot use SKU defaults: PL1/PL2
  setup-menu overrides and the optional package power-limit lock are not
  supported.
* E820 **ownership** stops at DPR; WB **cacheability** extends through DPR
  and TSEG up to BGSM. Graphics-stolen memory stays UC. Both PCI allocation
  and ACPI use TOM rather than TOLUD when ME starts at TOLUD.
* VT-d MMIO reservation (`0xfed9_0000`, 8 KiB) in `PCI0.PDRC` and `DMAR`
  table emission are both gated on `CAPID0_A.VTDD` (`vtd_capable_from_capid0_a`).
* Cold and S3 both program the GT power table, power well, panel, PWM, CDCLK
  and display codec. S3 only re-registers the retained OpRegion; it allocates
  neither a new framebuffer nor a new OpRegion. Haswell uses OpRegion **2.0**:
  an extended VBT address is physical, not relative. The 16-byte graphics
  signature and **four-byte** VBT build stamp match coreboot `opregion.c`;
  the rest of the 16-byte version field is zero. `brightness_levels` defaults
  to every percent; a board override is strictly increasing from 0 to 100.
  `_BCL` claims OS notification ownership.
* `device4` and `device7` retain the respective fuse-enabled SA functions;
  neither option overrides a fuse. Both default off.

## Lynx Point

* **ME9:** An ICC or End of Post timeout does not prevent SPI, LPC or TCO
  lockdown. LPT reports the EOP action, including action 1; this is not the
  pre-memory DID reset protocol. `me_disable` defaults on and hides MEI1 after
  eligible cold finalization, or before resource enumeration on S3. WPT-LP
  (ME10) is unsupported and rejected by LPC DID. An unknown stepping of a
  supported LPT skips only the stepping-specific HSIO tables.
* **Flash:** `smm_bios_protection` defaults off. When selected, SMM installation,
  BIOSWE clear, BLE, SMM_BWP and TCO SMI are required. With it off, handoff
  does not enable TCO SMI or open writes, and preserves any stronger existing
  protection. A firmware flash writer needs its own bounded authorization;
  there is no general SMM update service. The software-sequencer opcode menu
  supports ordinary byte/page-program SPI parts, not SST parts requiring the
  alternate AAI menu (`common/spi.c`; SST25VF064C is not an AAI part).
* **Interrupts:** PCI routing is APIC-only. A board selecting a PIC-only
  payload must set `pic_only_payload` and will fail construction; programmable
  PIC PIRQ links are not implemented. The board's ALT enables are additive.
  The chipset does not enable the RCBA monitor/I/O traps: there is no SMM
  monitor source to acknowledge at 0x1e00. Permanent LPT SMI routing does not
  turn on source 10. On H the two alternate GPI status words are acknowledged
  high first, both before and after the board hook.
* PM1 status and enable are independent 16-bit registers. FADT advertises a
  32-bit event block with **word** accesses, not a dword spanning W1C status
  and enables. A word suffices to change SCI_EN; sleep uses the chipset's
  dword PM1_CNT operation. EOS follows source acknowledgment and board work,
  but precedes generic rendezvous release (and CF8 restoration). Unlike
  coreboot, the owner does not drop the shared lock to write EOS. An entrant
  must acquire a new rendezvous generation before doing chipset work. Sticky
  pending sources are read anew, not cleared using the previous generation's
  snapshot; see `smm::handler::tests::only_the_first_owner_of_an_smi_is_its_first`.
* **USB:** LP has ten physical USB2 PHYs but only eight ordinary routable HS
  ports. On H, bank 2 maps ports 8–13 to bits 0–5 **within each OC byte**.
  Unlike `early_usb_native.c`, subtract eight from the port index: otherwise
  OC4 selects the OC5 field and OC7 overflows the register. This is the
  `U2OCM2` mask layout (six bits per pin), also used by Intel's PchUsbCommon
  `XhciHsOcm2` and `Ehci2Ocm` programming. `usb::tests::connectors` covers
  both ends and both pins. The H HCSPARAMS1 MaxPorts field occupies bits
  31:24, not 31:28. The EHCI-return path clears only the actual SS ports.
* SMM validates BARs against the allocator's saved assignments and does not
  enable bus mastering. EHCI stop uses `command & !RUN`, not coreboot's
  `command & RUN` (`usb_ehci.c`); `stop_preserves_command_except_run` checks
  that halting does not clear unrelated command bits. APM `0xca` runtime port
  switching is unsupported: the routing policy is logged at boot and a request
  emits POST `ca` and returns `0xff` at APM_STS. Native raminit has no S3 EHCI
  debug/MRC consumer, so there is no pre-RAM EHCI BAR handoff and no temporary
  DMA across training.
* **PCI and ACPI:** SATA map zero hides SATA; HDA `None` hides HDA. xHCI,
  EHCI, SMBus and thermal have board-level enables. `Some(0)` SATA DTLE is
  the coreboot zero sentinel (no PHY override). Board AML must test
  `serial_io_populated()` before emitting children: no BAR pair means no
  controller. BAR1 reservations belong to SIOR, not to the client driver.
  If the PCI-to-ACPI handoff fails after the snapshot on cold boot, omit that
  controller and withhold S3. On S3 the retained population, BDFs, BARs and
  handoff must all match; a failed handoff is not a usable resume.
  INTx line initialization covers bus zero and directly attached PCH endpoints;
  subordinate devices elsewhere need the platform's enumerated-device pass.
  LTR setup covers directly attached PCIe endpoints on the root links;
  configuring multi-level PCIe switch hierarchies is unsupported. S1 needs
  an explicit board policy.
