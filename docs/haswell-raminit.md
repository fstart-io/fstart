# Haswell native DDR3 raminit

This document describes the pre-memory Haswell DDR3/DDR3L memory initialization
driver in `crates/driver-intel/src/haswell/raminit/`. See
[Haswell / Lynx Point](haswell.md) for the full System Agent, Lynx Point PCH,
and `platform-intel` integration. The reference is coreboot `1958b5d923a`, in
`src/northbridge/intel/haswell/`. Both traditional and ULT use DDR3 mode
registers; soldered 1.35 V DDR3L is **not** LPDDR3. Like the reference, this
driver does not implement LPDDR3, Broadwell, A0, or XMP. There is no
Haswell-specific Cargo feature or binary MRC dependency.

## Integration contract

1. Run on the BSP from CAR. The platform must have applied microcode, set up
   ECAM, an enabled 32 KiB MCHBAR and its GDXCBAR (normally `0xfed84000`),
   the LPC RCBA and the DRAM power rails. Supply SMBus only for slotted
   memory. Set up ME, get ME UMA size and CPU-replacement status, perform
   early native PCH init, and do early DMI init on traditional parts.
   USB routing, PEG power limits, GPIO straps, and enabling SMRAM/CAR are
   platform or board work, not memory-training callbacks.
2. Construct `haswell::raminit::Board`. `slots` is **[channel][slot]**.
   Use `spd::Slot::SmBus(seven_bit_address)`, `MemoryDown(&[u8; 256])`,
   or `Empty`. No CBFS, filesystem, magic SPD address or GPIO driver is
   needed in the system agent. The board can point both channels at the
   same selected SPD image and resolve board-specific straps/EC revisions in
   board code via `HaswellConfig::select_spd`, not in the IMC driver.
   `configure_mc::Config` carries `ult`, `dq_pins_interleaved`,
   `double_refresh`, and optional effective RCOMP divider/target resistances.
   `rcomp: None` selects the reference's values, including the traditional
   2-DPC ODT adjustment. For DDR3 the DQ-interleave flag is the relevant
   routing option; LPDDR per-group DQ maps do not apply. ECC is determined
   from CAPID0_A and the DIMMs, not by a board override.
   `memory_map::Reservations` takes TSEG, DPR and ME sizes **in MiB** and an
   IGD-primary policy. Enabled IGD uses 64 MiB + 2 MiB GTT.
3. Sample `GEN_PMCON_2` before clearing DISB. Use
   `BootState::from_pmcon(pmcon2, s3resume, cpu_replaced)`; the platform
   clears DISB on initial cold entry and before resetting to recover from a
   failed resume. Supply `ddr3::ResetPin::SystemAgent` on traditional parts;
   on ULT use `LynxPointLp(&mut callback)`, where `callback(true)` **asserts**
   reset by clearing RCBA `PM_CFG2` (0x333c) bit 26, and `false` sets it.
4. Provide a zero-initialized, exclusive static `Workspace::EMPTY` (6,184 bytes)
   in CAR, not a stack temporary. Use `Board::cache_key(host_bridge, cpuid)` to
   load an integrity-checked record from the `fstart-store` training journal.
   Pass its payload slice (or `None`), not a journal header, to
   `unsafe { haswell::raminit::raminit(mch, &board, boot, bus, pin, cache, workspace) }`.
   `mch` is a `haswell::MchBar` wrapping the enabled `MmioWindow`.
   The driver checks CPUID, performs the TXT-clear wait and MSR 0x2e6 write,
   validates the cached payload against the full SPD of **each slot**, and
   dispatches cold or cached training. An invalid S3 cache is a reset, not
   permission to train over suspended memory.
5. Only `Ok(Initialized)` means the controller acknowledged initialization.
   The output includes the actual boot mode, clock, timings, decoded
   topology, physical-slot SPD, memory map and `memory_info()` for SMBIOS.
   Map fields are in MiB; `remap` uses an **inclusive** limit, `touud` an
   exclusive top. Do not use `tolud` as the end of usable RAM: IGD, GTT,
   TSEG and any DPR are below it. For coreboot `memmap.c` semantics, the
   SMM interval is `[tseg, gtt)` and the low allocator top is `dpr_base`
   with DPR.EPM enabled, otherwise `tseg`. `haswell::system_agent` and
   `platform-intel::haswell` publish the memory resources, install post-CAR
   MTRRs, and validate the resumed handoff.
6. Report `Initialized::memory_lost` to ME (Init Done status 3). It is set
   if DRAM entered in self-refresh and the schedule reset it. Even successful
   warm replay pulses DRAM_RESET# and reissues MRS: it preserves **training,
   not memory contents**. Only S3 can use a retained-memory handoff. On a
   non-S3 success, publish/copy `identity` and `save_data` before tearing CAR
   down and let `fstart-store` persist them from DRAM. Do not persist failed
   or S3 training. Provision `CACHE_BYTES` (3,560 bytes) plus journal overhead.
   A cache payload is fstart-specific, **not** compatible with a coreboot
   register-save frame.

## Source map

Rust paths are relative to `crates/driver-intel/src/haswell/raminit/`
except where noted. Each training stage below performs hardware operations;
none are delegated to an unspecified board training hook.

| Coreboot source | Rust implementation |
|---|---|
| `raminit_native.[ch]`, `raminit_main.c` | `flow.rs`, `mod.rs`, `configure_mc::TrainingState`; outer ME/PCH/reset/store policy lives in `../system_agent.rs` and `platform-intel/src/haswell.rs` |
| `reg_structs.h`, `registers/{mchbar,host_bridge}.h` | `../registers.rs` (typed 32/64-bit layouts); offsets/strides at the programming sites; `../mod.rs` wraps `MmioWindow` in `MchBar` |
| `raminit_shared.c`, `spd_bitmunching.c`, `device/dram/ddr3.c` | `spd.rs`, `timings.rs`, `flow.rs`, `info.rs`; shared `generic::spd::ddr3` SMBus transport and `fstart_driver_spd::ddr3` decoder/inventory |
| `init_mpll.c`, `lookup_timings.c`, `timings_refresh.c` | `timings.rs` |
| `configure_mc.c`, `io_comp_control.c` | `configure_mc.rs`, `io_comp.rs`, reset/RCOMP polling in `mod.rs` |
| `memory_map.c` | `memory_map.rs` |
| `ddr3.c`, `jedec_reset.c` | `ddr3.rs` |
| `reut.c`, `testing_io.c`, `setup_wdb.c` | `reut.rs`, `testing_io.rs`, `wdb.rs` |
| `ranges.[ch]`, `change_margin.c` | `ranges.rs`, `change_margin.rs`, `io_comp.rs` |
| `train_sense_amp_offset.c` | `sense_amp.rs` |
| `train_receive_enable.c` | `receive_enable.rs` |
| `train_read_mpr.c`, `train_1d_margins.c` | `centering.rs` |
| `train_jedec_write_leveling.c` | `write_leveling.rs` (includes whole-cycle cleanup) |
| `train_late_command.c` | `late_command.rs` (2D CTL/CLK sweep, CA Vref stress, binary edges, rank-group centering, normalization) |
| `activate_mc.c`, pre-/post-training from `raminit_main.c` | `activate.rs`, `flow.rs` |
| `memory_scrub.c` | `memory_scrub.rs` |
| `save_restore.c` | `save_restore.rs` (624 ordered dwords including the ninth lane on both channels), `flow.rs` (frame and acceptance) |
| `info_dumps.c` | `info.rs` |

## Deliberate differences and limitations

* Use the existing SPD decoder's MTB/FTB arithmetic and CAS bitmap (bit N =
  CL N). Program the **selected supported CAS**: the C clock search finds
  it but then recomputes tAA without checking holes in the CAS mask. Reject
  clocks with unencodable CWL, CL−CWL or tRRD. Validate the SPD CRC, board
  supply, module geometry, density/addressing agreement, and CAPID0_A
  channel/DPC/capacity limits before MAD or a destructive scrub. Bus errors
  propagate; an address NAK denotes a wired empty socket in SMBIOS. No XMP data is required.
* Fix the C precedence issue `vref & 0xf << (4 * bit)` when packing the eight
  sense-amp offsets. Keep whole-cycle PI adjustments wide during write-level
  refinement. Use the physical slot (`rank / 2`) for DIMM MR shadows even for
  ranks 2/3. Apply `WrLevel` to both TX fields and download the write regfile;
  the C switch omits that case. For the optional all-read turnaround test,
  keep the linear-scale bit when selecting the rank update rate.
* Waits for TXT clear, REUT completion and ECC scrub have deadlines. A busy
  MPLL must complete or return `PllBusy` (reset required; no cold retry).
  A locked MPLL is never reprogrammed: match the REQ reference and DATA
  multiplier to a compatible bin. A stuck I/O reset, failed initialization
  MRS/ZQ, narrow eye, or missing init-done acknowledgement prevents
  publication. As in coreboot, a REUT channel error ends a CADB command
  wait (only neither done nor error is a stuck controller); during command
  probes it fails only that channel's sample, and on self-refresh exit a
  ZQ_LONG error is a warning. Also as in coreboot, a slow-settling Vref
  circuit and a 2x-refresh PCODE mailbox failure or foreign lock are
  reported, not fatal. Between 2D command sweeps, restore coupled DQ and
  clock PIs together. A late-command clock sweep with no failing sample is
  coreboot's good `[0, 126]` eye, not a failure.
* Cache identity includes CPU, fuses, wiring, refresh/compensation and memory
  reservations. Parameters derived from unchanged SPD/fuses are recomputed
  rather than serializing C structs with padding. Recompute the ASR tREFI
  reduction once on each path. Changing CPU forces cold training rather
  than accepting a cache. Cached failure on fast/warm entry can retry cold;
  **S3 never falls back to a destructive schedule**. Fast and warm replay
  initialize ECC parity after restoring the controller; S3 never scrubs.
  The v2 cache key and `HSW2` frame magic reject pre-fix rank training.
* The controller runs the active DDR3/DDR3L B0/C0 sequences. Unreachable
  LPDDR3-specific branches and hard-stop stubs from C are not provided.
  No invented `pei_data` ABI: the binary MRC's USB tables and max-frequency
  selector are not inputs to coreboot's native algorithm. Unless overridden,
  supply voltage and compensation match the native ULT/traditional choices.
* Diagnostic output is register/step-oriented; the C timing summary and
  ASCII per-bit shmoo plots are not reproduced. REUT bit-lane readback and
  per-eye margins are implemented without building a plot history on CAR's
  stack. As in coreboot, MMIO CADB writes must be a single 64-bit transaction;
  i686 uses MMX with mm0 preserved. Shared `udelay`/`wait_us` use Haswell's
  non-turbo ratio and 100 MHz BCLK rather than a 1 GHz fallback.

Tests check SPD, CAS/frequency selection, ECC resolution, mode/timing/refresh
and MAD encodings, margin math, wraparound, nibble packing, register-frame
order, cache acceptance, and resume rejection. RAM-backed MMIO tests exercise
the programming code; they are **not** a controller/DRAM electrical model.
An independent host check compiled the packed coreboot C unions with Clang,
set each named field to all ones, and compared the resulting raw word with
its Rust bitfield mask: all 521 fields in 65 register definitions matched.

**Bring-up still required:** board SPD strap maps, cold/fast/warm/S3 tests on
ULT and traditional DIMM systems, and ECC hardware testing. Existing Intel
regression board builds do not exercise Haswell.
