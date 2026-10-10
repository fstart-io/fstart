# Pineview boards

The Pineview/NM10 boards use `PineviewIch7<Fcbga559>` and the existing Atom
CPU, DDR2, graphics, PCI and ICH7/SMM flows. `fbuild` discovers their board
crates; no generated register scripts are needed.

| Selection | Super I/O | SPI image capacity | Status |
| --- | --- | --- | --- |
| `foxconn-d41s` | IT8721F at 0x2e | 16 MiB development layout | Existing port, halt image assembles |
| `intel-d510mo` | W83627THG at 0x4e | 1 MiB | Halt image assembles; no hardware validation |
| `gigabyte-ga-d510ud` | IT8720F at 0x2e | 512 KiB per BIOS bank | Halt image assembles; no hardware validation |

Build stages with `cargo fbuild build -b <selection> --release --payload halt`.
Assemble an image with `cargo fbuild assemble -b <selection> --release --payload halt`.
These new ports are not hardware-tested firmware releases. Keep a complete vendor ROM
backup and an external programmer before trying an image. A successful build
does not establish DRAM training, S3, display or peripheral correctness. Image
capacity also constrains payload choice; the halt payload is a small smoke
build, not proof that a larger UEFI payload fits the stock flash.

## Board policy versus hardware encoding

* `PirqRouting` owns function/pin/PIRQ relationships and produces both RCBA
  programming and ACPI routes. D510MO supplies its PCI-slot G/E/B/A wiring;
  GA-D510UD uses the existing F/G/H/E and slot-1 D routing.
* The Pineview builder exposes integrated-LAN disable policy and early serial
  IRQ admission. D510MO keeps serial IRQs disabled until mainstage, then the
  common ICH7 driver enables continuous framing.
* D41S uses the shared named-device Super I/O configuration directly, with
  separate keyboard/mouse logical devices and the two ITE monitoring windows.
  IT8721F operations explicitly disconnect the monitoring IRQ, disable parallel
  ECP decoding, and deactivate the unused floppy logical device. GPIO
  configuration is untouched: the reference has no GPIO mux script and its
  GPIO-off declaration does not establish an IT8721F activation-register write.
* The W83627THG descriptor masks the chip-ID revision nibble, uses two IRQ
  slots in the combined KBC/mouse LDN, and treats HWMBASE as a single resource,
  rather than misusing the ITE environment-controller layout. KBC clock and
  UART-B IR control use datasheet-defined Tock fields.
* IT8720F GPIO mux, simple-I/O selection, direction and pull-ups are expressed
  per physical pin (`GpioPinConfig::input(40)` means GP40, not bit 0 of group 4).
  Tock field-level updates leave unlisted pins and reserved bits alone; GP45 and
  unsupported pull-ups are rejected. `simple_io(22)` retains the strap direction,
  while `input(40)` explicitly selects input. Driver operations own watchdog
  shutdown, internal power-good selection, named BSEL latch levels and BIOS-bank
  selection.
* GPE policy lists named events (`TcoSci`, `Thermal`, etc.) and physical GPIO
  numbers: `Ich7Gpe::Gpio(13)` encodes GPE 29 inside the driver. SATA policy lists
  connected port numbers (`SataConfig::new(SataMode::Ahci, [0, 1])`), not a bitmap.
* Shared `IndexedPioRegister` implements Tock `Readable`/`Writeable`, so indexed
  chip accesses use the same field operations as MMIO. Callers own the chip's
  configuration-mode session and must serialize the index/data port pair.

## Deliberate IT8720F reference differences

Coreboot's GA-D510UD early initialization includes two misleading writes:

* LDN7/FC = 0xa4 is labelled only by its register number. IT8720F V0.1 §8.11.36
  defines FC as VID **input**, read-only except the bit-0 reload strobe. The
  reference write has that bit clear and cannot set VID; it is omitted.
* LDN7/EF = 0x7e is described as disabling Super I/O reboot. Flashrom's IT87
  DualBIOS support establishes that bit 0 selects the BIOS bank, but the older
  datasheet does not define the other bits. The driver selects the main bank
  with a field-level read/modify/write and preserves those undocumented bits;
  it does **not** claim the unexplained upper bits have reset semantics.

The latter is not proven bit-for-bit equivalent to the coreboot workaround.
Validate warm reset and main/backup flash selection on GA-D510UD before treating
this as a completed hardware port. There is also no dedicated Realtek 8168
firmware reset/MAC-loading driver or JMB363-specific initialization yet; generic
PCI enumeration and BAR allocation are not substitutes for those routines.

## References

Board wiring and VBT data: coreboot `src/mainboard/intel/d510mo`,
`src/mainboard/gigabyte/ga-d510ud` and `src/mainboard/foxconn/d41s`.
Register meanings: Intel ICH7 Family Datasheet §10.1.18, Winbond W83627THF
Rev. 0.8 §9.1.4–9.1.5, ITE IT8720F V0.1 §8.3/§8.11, and flashrom `it87spi.c`
(DualBIOS bank selection).
