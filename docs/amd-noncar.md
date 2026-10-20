# The AMD noncar flow

The PSP-booted AMD SoCs from Family 17h on share one boot flow: the
PSP trains DRAM, writes
the APOB and copies the BIOS reset image (directory entry 0x62) to DRAM
before it releases the BSP at the image's last 16 bytes. There is no
cache-as-RAM. One stage runs from the reset vector to the payload.

`crates/platform-amd/src/noncar` holds the flow; a SoC (`zeppelin`,
`picasso`) implements `noncar::Soc`, a board `noncar::Board`.

## Flow

`noncar::flow` owns the order and the common steps. The SoC's hooks are
openSIL's timepoints (TP1-TP3); the flow keeps what lies beside
openSIL: enumeration, resources, ACPI, the memory map, SMM and MP.

| Step | Owner |
| --- | --- |
| ECAM, ACPI MMIO, TSC frequency | flow |
| FCH pre-init: the console's decodes | `Soc::fch_pre_init` |
| LPC decodes, Super I/O | `Board::early_init` |
| Console | flow, `Board::console` |
| CPU identification, early FCH | `Soc::identify` |
| TOP_MEM, APOB memory map, TSEG, e820, cache plan | flow (`memory`) |
| TP1: the silicon before PCI enumeration | `Soc::silicon_init` |
| PCI enumeration and resource allocation below die 0 | flow (`pci`) |
| TP2 | `Soc::after_resources`, then `Board::mainboard_final` |
| FCH and NBIO IOAPICs | flow |
| ACPI tables | flow and `crate::acpi`, `Board::acpi` |
| TP3: ready to boot | `Soc::ready_to_boot` |
| PSP LockDFReg, FFS, page tables, AP parking, PSP BootDone, payload | flow (`psp`, `boot`) |

TP1 includes MP init and SMM where the SoC does them before PCI
enumeration: it calls the shared `noncar::mp::start` (SMU AP release,
microcode and the SoC's CCX init on every CPU), `noncar::smm::install`
and `noncar::psp::notify_smm`, and returns them in `Silicon` with what the
later steps need: the NBIO IOAPICs and the reserved MMIO per die, the SoC's
ACPI data and its own `State`. The board hooks of the silicon phases
(`fch_reset_done`, `dxio_training_done`, `ready_to_boot`) are called by the
SoC where its sequence has them.

A SoC whose later phases aren't ported stops TP1 with `Stop::Checkpoint`
and makes its `State` uninhabited (`Infallible`), so the types show that
nothing after it runs.

## Configuration

A board names its SoC and its configuration once:

```rust
impl fstart_platform_amd::noncar::Board for Board {
    type Soc = Zeppelin;
    const CONFIG: &'static zeppelin::Config = &CONFIG;
    fn console() -> Ns16550Config { ... }
}
```

The SoC's `Config` has a typed part per IP block (`zeppelin::FchConfig`,
`DxioConfig`; `picasso::FchConfig`) with `Default` where a default exists.

## Layout and image

`noncar::layout::Layout` is a SoC's DRAM layout: the early region reserved
through OS handoff, page tables, data and BSS, heap, stack, the PSP-copied
image, the boot flash and the FFS, if any. The one linker script generator,
the layout descriptor and the ELF expectations derive from it
(`noncar::host`). `asm/psp_reset.S` sends the APs to the SIPI trampoline at
`0x8000`, copies `.data` out of the image, clears BSS and enters long mode;
the image bytes the PSP copied are never written. The flow continues at the
architectural `_start_ram`.

## ACPI

`crate::acpi` emits the tables (FADT, MADT, HPET, MCFG, IVRS, the DSDT with
a root bridge per die, bridge `_PRT`s and the FCH's LPC devices) from data:
the SoC's `SocAcpi` (FCH ACPI ports, FADT fields, the NBIO's bridge
interrupt routing, the FCH INTx GSIs), what the flow found (CPUs, dies,
IOAPICs, IOMMUs, bridges, windows) and the board's `BoardAcpi`.

## Multi-die

The interfaces take every die (`Silicon` has per-die IOAPICs and reserved
MMIO, ACPI a root bridge per die), but the flow enumerates PCI below die 0
only, as before.
