# Options

Users override a board's configuration through `fstart-options`
(`crates/options`): typed knobs, stored values read by name, and the
setup menu of the UEFI payload (CrabEFI's CFR firmware settings).

## Knobs

A `Knob<C>` is one field of a configuration `C`: its key, menu form,
label, legal values (`Kind`), how to read and set it, and whether the
running machine lets the user change it (`Availability`). The code that
consumes a field defines its knob; a board only picks the knobs it offers.

| Knobs | Owner | Configuration |
|---|---|---|
| `fstart_log::option::KNOBS` (`debug_level`) | log | console level |
| `southbridge::options` (`power_on_after_fail`, `sata_mode`, `nmi`) | ICH7/8/9, Ibex Peak | `IntelSouthbridgeDriver::Options` |
| `igd::options` (`gfx_uma_size`, `tft_brightness`) | i945, Pineview, GM965, GM45, Ironlake | `IntelNorthbridgeDriver::Options` |
| `fstart_driver_lenovo::options` (`wlan`, `bluetooth`, `trackpoint`, `volume`, ...) | H8 EC, PMH7 | the ThinkPad's `EC_KNOBS` |
| `fch::common::env::PowerFail`, `lanes::Bifurcation::meta` | SP3 FCH, DXIO | `Policy::fch_env`, the board's lane map |

Keys and values follow coreboot's option names where coreboot has the
option; sizes are in MiB rather than coreboot's per-chipset indices.

## What a build offers

A build describes its own options: `fbuild options <stage ELFs>` prints
each option's key, form, label, help, legal values, board value and
availability as JSON. There is no list per board.

- A knob is a type made by `fstart_options::knob!`, with `const fn`
  accessors. `Knob` and `Record` values only come from the macros, so
  every knob a stage resolves has a record.
- `fstart_options::option_set!` declares the knobs a configuration offers
  (the ThinkPads' `EC_KNOBS`, the S8030's slots, the log level) and writes
  the set's `Record`s, in the order the set lists them, as one array into
  its own `.rodata.fstart.options.*` section: each knob's `Meta`, its
  value on the set's constant configuration and whether that
  configuration offers it, evaluated at build time. A set over a
  configuration the board detects (Ethanol-X's straps) records what it
  can and says the rest is decided at boot.
- The Intel chipset's records come from the platform config's
  `option_records`, which `entry!` writes into each stage for its `Board`;
  `options::chipset` references them.
- The linker keeps the records a stage's knobs reference and drops the
  rest, and fstart's linker scripts collect them between
  `_fstart_options_start` and `_fstart_options_end`. So the X61 offers
  the radios its EC set lists but not the X200's UWB.

A build's options are its last stage's: the stage that hands the menu to
the payload resolves every option, the earlier stages only those they
apply early (the Intel bootblock's chipset knobs, every stage's
`debug_level`). `fbuild options` takes the stages in boot order, fails if
an earlier stage resolves an option the last one does not, or describes
it differently, and keeps the last stage's order.

An option whose configuration lacks what it changes is
`Availability::Hidden` (the D945GCLF's panel brightness: it has no panel,
an Ethanol-X slot its fuses or straps remove). It is left out of the
build's options and of the payload's menu, and a stored value for it is
ignored.

## Resolution

`resolve_each` applies the stored values of a knob set to its
configuration, runs the consumer's whole-set check (for example the DXIO
lane validation) and drops every override of the set if it fails, so a bad
combination boots the board's values. It hands each knob's outcome to the
caller and allocates nothing: pre-DRAM stages use it.
`fstart_log::option::resolve` logs it. With the `alloc` feature,
`resolve_checked` keeps the outcomes in a `Report`, which the stage hands
to the payload as a CFR form for its menu; a knob whose key is already in
the report is skipped and logged.

## Stores

A `Source` reads stored values by key; `Layered` reads one before another.

- **Flash variable store** (`varstore` feature, SP3): CrabEFI's EDK2
  variable store at the top of the FFS window, one variable per option
  under coreboot's CFR GUID holding a little-endian `u32`. Every payload's
  boot reads it through the ROM window; CrabEFI's menu writes it.
- **CMOS** (`cmos`, Intel CAR platforms): a window of RTC RAM bank 0 the
  platform leaves free (`platform-intel` uses `0x40`..`0x7f`). An override
  is a record of its key's 16-bit hash and its value, so nothing about the
  options is declared, only changed values take space and later firmware
  reads the same records. The window's last two bytes hold its byte sum;
  while it does not match (new board, dead battery, cleared CMOS) the
  board's values boot. Tests check that a board's keys have distinct
  hashes (`cmos::collision`).

## Stages on the Intel CAR platforms

The chipset drivers declare `Options` and `KNOBS`; the platform applies
them in the stages that run the driver, before its early init, reading
CMOS without allocating:

| Stage | Knobs |
|---|---|
| bootblock | `debug_level`, northbridge (stolen memory is reserved in raminit), southbridge (SATA mode) |
| postcar | none: the bootblock hands it its console level in the post-CAR stash |
| ramstage | the same, into the report; board knobs through `Ramstage::resolve_options` (the ThinkPads' EC knobs) |

With the UEFI payload the ramstage hands CrabEFI the CFR form and the CMOS
store; the menu writes the options there and they apply at the next boot.
