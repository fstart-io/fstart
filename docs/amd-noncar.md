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
| Mount and verify the FFS, its microcode for MP init | flow (`boot`) |
| TP1: the silicon before PCI enumeration | `Soc::silicon_init` |
| PCI enumeration and resource allocation below every die | flow (`pci`) |
| TP2 | `Soc::after_resources`, then `Board::mainboard_final` |
| FCH and NBIO IOAPICs | flow |
| ACPI tables | flow and `crate::acpi`, `Board::acpi` |
| TP3: ready to boot | `Soc::ready_to_boot` |
| PSP LockDFReg, page tables, AP parking, PSP BootDone, payload | flow (`psp`, `boot`) |

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

`noncar::host` is also the one build plan of every noncar SoC: the stage
unit `reset`, the board's SMM unit, the payloads and the FFS follow from the
layout and the SoC's `host::Policy` (its stage feature, whether the flow
reaches a payload, SMM, what `--image-config` is).

The flash image format (the EFS, the PSP and BIOS directories and their
level 2) is `crates/amdfw` (`fstart-amdfw`, `no_std`): the host packs and
checks images with it and takes the microcode from it. A SoC names the
EFS field its PSP takes the BIOS directory from (`Soc::BIOS_DIRECTORY`); the
lookup follows a level-2 directory as the PSP does.
`crates/image-build/src/amd/assembly.rs` (`AmdAssembly`) composes the image
with `--image-config`: the PSP firmware comes from a vendor flash image,
whose prefix stays byte for byte apart from optional PSP settings (Zeppelin
has the PSP inflate a compressed reset image and read the flash faster:
`assembly::PspOptions`), or is packed from blobs
(Picasso). Either way the result is read back as
the PSP would before it is written, with a JSON receipt of its inputs.

The microcode travels in the FFS, like Intel's: the host concatenates the
patches into `cpu_microcode.bin` (the BIOS directory type 0x66 entries of a
vendor image, or the type 0x66 files of a packed configuration;
`MicrocodeConfig::Amd`). The flow mounts and verifies the FFS before TP1, and
MP init hands every CPU the verified copy (`fstart_stage::microcode_blob`),
from which `Fam17hCpu` selects the patch for its signature.

## ACPI

`crate::acpi` emits the tables (FADT, MADT, HPET, MCFG, IVRS, the DSDT with
a root bridge per die, bridge `_PRT`s and the FCH's LPC devices) from data:
the SoC's `SocAcpi` (FCH ACPI ports, FADT fields, the NBIO's bridge
interrupt routing, the FCH INTx GSIs), what the flow found (CPUs, dies,
IOAPICs, IOMMUs, bridges, windows) and the board's `BoardAcpi`.

## PCI root bridges

Every die's root complex is a PCI root bridge. What the DF routes to the
die's IOMS is its one source of truth: `df::map::root_complex` reads its
bus map and its MMIO and I/O windows, the ones the SoC programmed
(`df::map::program` on Zeppelin) or the PSP left. `noncar::pci::Root`
turns that into the root bridge: the buses clamped to the ECAM, the I/O
above the legacy 4 KiB and the MMIO below 4 GiB without the ECAM, the
fixed MMIO from the IOAPIC up and the die's reserved MMIO. The 64-bit DF
windows start at 1 TiB, beyond what the firmware maps, and aren't part of
the root bridges yet.

One `PciEcam` enumerates and allocates below all root bridges, each from
its own buses and windows, and the ACPI host bridge of each die (`PCIn`
with `_BBN` and `_CRS`) describes the same root.
