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
| `southbridge::options` (`power_on_after_fail`, `sata_mode`, `nmi`) | ICH7/8/9 | `IntelSouthbridgeDriver::Options` |
| `igd::options` (`gfx_uma_size`, `tft_brightness`) | i945, Pineview, GM965, GM45 | `IntelNorthbridgeDriver::Options` |
| `fstart_driver_lenovo::options` (`wlan`, `bluetooth`, `trackpoint`, `volume`, ...) | H8 EC, PMH7 | the ThinkPad's `EC_KNOBS` |
| `board::knob::POWER_FAIL`, `lanes::Bifurcation::knob` | SP3 FCH, DXIO | `Policy`, the board's lane map |

Keys and values follow coreboot's option names where coreboot has the
option; sizes are in MiB rather than coreboot's per-chipset indices.

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
them in every stage that runs the driver, before its early init, reading
CMOS without allocating:

| Stage | Knobs |
|---|---|
| bootblock | `debug_level`, northbridge (stolen memory is reserved in raminit), southbridge (SATA mode) |
| postcar | `debug_level` |
| ramstage | the same, into the report; board knobs through `Ramstage::resolve_options` (the ThinkPads' EC knobs) |

With the UEFI payload the ramstage hands CrabEFI the CFR form and the CMOS
store; the menu writes the options there and they apply at the next boot.
