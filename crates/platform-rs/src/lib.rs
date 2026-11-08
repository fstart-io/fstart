// SPDX-License-Identifier: MIT OR Apache-2.0
//! The AMD Rs SoCs, Genoa (SP5) and Siena (SP6): PSP reset through payload
//! handoff on the shared `fstart_platform_amd::rs_*` sequences. A board
//! names its SoC ([`Board::Soc`]) and enables the matching feature, which
//! selects the reset image's geometry. What differs per SoC is data
//! ([`Soc`]): layout, vendor policies, fuses and identification.
#![no_std]
#[cfg(feature = "bundle-reset")]
extern crate alloc;
#[cfg(feature = "host")]
extern crate std;

#[cfg(feature = "genoa")]
pub mod genoa;
#[cfg(feature = "host")]
pub mod host;
#[cfg(feature = "siena")]
pub mod siena;
#[cfg(feature = "genoa")]
pub use genoa::Genoa;
#[cfg(feature = "host")]
pub use host::Plan;
#[cfg(feature = "siena")]
pub use siena::Siena;

pub use fstart_core::layout::Region;
pub use fstart_driver_amd::fch::songshan::uart::Uart;
use fstart_driver_amd::{
    fch::songshan::usb,
    mpio::{mailbox::Encoding, session},
};

/// One-socket Rs storage bound, not a discovery result (Genoa: 12 CCDs x8
/// x SMT2). CPU identities and counts come from the checked APOB topology.
pub const MAX_CPUS: usize = 192;

/// Eight PDPTs identity map 4 TiB. MMIO is UC outside the cache window.
pub const PDPT_COUNT: u64 = 8;

/// A SoC's DRAM layout: the reset image and APOB destinations the packed
/// BIOS directory names, and the firmware's own regions.
#[derive(Debug, Clone, Copy)]
pub struct Layout {
    /// The reset image at its PSP copy destination.
    pub reset: Region,
    /// Bounded APOB aperture, not an assertion about its actual size.
    pub apob: Region,
    /// Firmware RAM below the reset image: page tables, data, heap, stack.
    pub early: Region,
    pub pages: Region,
    pub data: Region,
    pub heap: Region,
    pub stack: Region,
    /// The write-back MTRR window over the early region and the image.
    pub cache: Region,
    /// CrabEFI's freeing heap (`fstart_stage::adopt_freeing_heap`),
    /// boot-services memory the OS reclaims after ExitBootServices.
    pub payload_heap: Region,
    /// Linux boot_params and command line, kept out of OS allocation.
    pub linux_params: Region,
}

/// What a board's SoC is: its layout, identity and vendor policies.
pub trait Soc {
    /// Log prefix and build-plan name.
    const NAME: &'static str;
    const LAYOUT: Layout;
    /// SMBIOS processor socket designation.
    const SOCKET: &'static str;
    /// The ACPI PM base the vendor's FCH init programs; the primary root
    /// keeps the ACPI blocks when the I/O space is partitioned.
    const PMBASE: u16;
    /// The SMU's USB message-ID limit.
    const SMU_ID_LIMIT: u32;
    /// The microcode equivalence-ID family byte.
    const EQUIVALENCE: u8;
    const LINUX_REQUIRES_MICROCODE: bool;
    /// The bound for a ROM Linux kernel inside the reset image.
    const LINUX_CAPACITY: u64;
    /// CPUID family/model and the package type.
    fn is(signature: u32, package: u8) -> bool;
    /// The live IOS roots from the SoC's fuses, checked against the DF's
    /// socket and CPU identity. `Err` holds the POST code to stop with.
    #[cfg(all(feature = "bundle-reset", target_os = "none"))]
    fn fabric() -> Result<heapless::Vec<fstart_driver_amd::df::v4::Root, 4>, u8>;
}

/// The board's console UART.
#[derive(Debug, Clone, Copy)]
pub enum Console {
    /// An FCH (SoC) UART: MMIO, 48 MHz, its legacy IRQ 4 routed from its
    /// FCH interrupt source.
    Fch(Uart),
    /// COM1 (3F8h, 1.8432 MHz) of the board's Super I/O at 2Eh over LPC;
    /// [`Board::early_init`] configures the Super I/O. Its IRQ arrives over
    /// SERIRQ.
    SuperIoCom1 { irq: u8 },
}

/// Board wiring and physical NOR facts. The image is packed from blobs
/// (`--blobs`/`--donor`), never composed into a vendor image.
pub trait Board {
    type Soc: Soc;
    const NAME: &'static str;
    const SMBIOS_MANUFACTURER: &'static str = "Unknown";
    const SMBIOS_PRODUCT: &'static str = Self::NAME;
    /// Reporting capacity ceiling per DIMM, not population or a DIMM QVL.
    const SMBIOS_MAX_DIMM_MIB: u32;
    /// The UMC channels (bit per channel) of the board's DIMM slots; the
    /// APOB describes all twelve of the IOD's.
    const MEMORY_CHANNELS: u16;
    /// The host NOR part; the packed image covers all of it.
    const FLASH_SIZE: u32;
    const CONSOLE: Console;
    const BAUD: u32 = 115_200;
    /// NBIO clock gating and TPH completion; all off by default.
    const NBIO: fstart_driver_amd::nbio::rs::Policy = fstart_driver_amd::nbio::rs::Policy {
        clock_gating: false,
        syshub_clock_gating: false,
        tph_completer: false,
    };
    /// The CPU watchdog (CpuWdtCfg time base, count select) to enable, if any.
    const WATCHDOG: Option<(u8, u8)> = None;
    /// Write-combining speculation, or the reset value.
    const WC_SPEC: Option<bool> = None;
    /// The ACPI boot architecture flags: LPC/ISA legacy devices present.
    const LEGACY_DEVICES: bool = matches!(Self::CONSOLE, Console::SuperIoCom1 { .. });
    /// The kernel command line of a ROM Linux payload without explicit
    /// arguments.
    const LINUX_BOOTARGS: &'static str;
    /// Required soldered endpoints: board lane and PCI vendor, never BDF.
    const REQUIRED_PCIE: &'static [(u8, u16)] = &[];
    /// Retained PSP-trained paths to own without retraining: physical IOS,
    /// downstream PCI vendor/device. Match topology, never a guessed BDF.
    const RETAINED_PCIE: &'static [(u8, u16, u16)] = &[];
    /// Bounded per-root low-MMIO budget, not a discovered demand maximum.
    const PCI_ROOT_MMIO_SIZE: u64 = 0x800_0000;
    /// Per-root above-4-GiB budget for 64-bit BARs, from 1 TiB.
    const PCI_ROOT_MMIO64_SIZE: u64 = 1 << 38;
    /// The display (PCI vendor, device) the legacy VGA aperture goes to.
    const PRIMARY_VGA: Option<(u16, u16)> = None;
    /// The retained MPIO firmware vets requests with the PSP. Selected
    /// before any request; never changed on failure.
    const MPIO_PSP_VETTING: bool = true;
    const MPIO_ENCODING: Encoding = Encoding::Retained;
    /// The SMU firmware revision the USB policy was measured with, if the
    /// board pins one.
    const USB_SMU_VERSION: Option<u32> = None;
    /// The OEM AMD CPM GPIO table and the board's policy-driven pads:
    /// (logical pin, CPM setting), applied in order before MPIO, as
    /// AmdCpmGpioInitPeim and the OEM PEI callbacks do.
    fn cpm_gpio() -> heapless::Vec<(u16, u16), 64> {
        heapless::Vec::new()
    }
    /// Board setup once the platform decoded the console's Super I/O.
    /// # Safety
    /// The BSP owns the decoded Super I/O configuration ports.
    unsafe fn early_init() {}
    /// Explicit cold-boot connector policy. Missing policy fails closed.
    fn mpio_ports() -> Result<heapless::Vec<session::Config<'static>, 32>, session::Error> {
        Err(session::Error::Unsupported)
    }
    /// Explicit board USB wiring/tuning, never an older SoC's defaults.
    fn usb_policy() -> Option<usb::Policy> {
        None
    }
    /// Release the board's verified physical PERST outputs.
    /// # Safety
    /// BSP-exclusive FCH access, single socket, MPIO PERST phase complete.
    unsafe fn release_perst() -> Result<(), &'static str> {
        Err("missing PERST policy")
    }
}

#[cfg(any(feature = "genoa", feature = "siena"))]
/// Family 19h with `model` in `models` and the package type, as CPUID
/// reports them.
const fn is_family19(signature: u32, package: u8, models: (u32, u32), want: u8) -> bool {
    let base_family = (signature >> 8) & 15;
    let family = if base_family == 15 {
        base_family + ((signature >> 20) & 255)
    } else {
        base_family
    };
    let model = ((signature >> 4) & 15) | ((signature >> 12) & 0xf0);
    family == 0x19 && model >= models.0 && model <= models.1 && package == want
}

/// The shared PSP reset entry, with the selected SoC's geometry.
#[cfg(all(target_os = "none", feature = "bundle-reset"))]
mod entry {
    #[cfg(all(feature = "genoa", feature = "siena"))]
    compile_error!("a reset image is for one SoC: enable genoa or siena, not both");
    #[cfg(not(any(feature = "genoa", feature = "siena")))]
    compile_error!("a reset image needs its SoC: enable genoa or siena");
    #[cfg(feature = "genoa")]
    use crate::genoa::LAYOUT;
    #[cfg(feature = "siena")]
    use crate::siena::LAYOUT;
    core::arch::global_asm!(
        include_str!("../../arch/asm/amd_psp_reset.S"),
        cache_base = const LAYOUT.cache.base,
        cache_mask = const !(LAYOUT.cache.size - 1) & 0xffff_ffff,
        pdpts = const crate::PDPT_COUNT,
        options(att_syntax),
    );
}

/// A board's `smm.rs`: `smm::smm_bin!(handler: rs_smi_handler)`.
#[cfg(feature = "smm")]
pub use fstart_platform_amd::{rs_smm::handler::smi as rs_smi_handler, smm};
#[cfg(feature = "bundle-reset")]
#[doc(hidden)]
pub use fstart_stage as stage_runtime;
#[cfg(all(feature = "payload-linux", feature = "payload-uefi"))]
compile_error!("one payload per reset image");
#[cfg(all(feature = "bundle-reset", target_os = "none", feature = "payload-uefi"))]
mod boot;
#[cfg(all(feature = "bundle-reset", target_os = "none"))]
mod flow;

/// The board's reset image: from the PSP's release of the BSP to the payload.
#[cfg(all(feature = "bundle-reset", target_os = "none"))]
pub fn run<B: Board>() -> ! {
    flow::run::<B>()
}

/// The board's stage binary: `fstart_platform_rs::stage_bin!(Board);`.
#[cfg(feature = "bundle-reset")]
#[macro_export]
macro_rules! stage_bin {
    ($board:ty) => {
        fn fstart_platform_rs_main() -> ! {
            $crate::run::<$board>()
        }
        $crate::stage_runtime::stage_bin!(main: fstart_platform_rs_main);
    };
}
#[cfg(not(feature = "bundle-reset"))]
#[macro_export]
macro_rules! stage_bin {
    ($board:ty) => {
        ::core::compile_error!(
            "the Rs reset image needs fstart-platform-rs/bundle-reset (fbuild enables it)"
        );
    };
}

#[cfg(all(test, any(feature = "genoa", feature = "siena")))]
mod tests {
    use super::*;

    /// Every SoC's layout: firmware storage, APOB and image disjoint; the
    /// early RAM ends at the stack; the cache window is aligned and covers
    /// the early RAM and the image; the payload's regions stay outside.
    pub fn layout_is_consistent(layout: &Layout) {
        let end = |r: Region| r.end().unwrap();
        let disjoint = |a: Region, b: Region| end(a) <= b.base || end(b) <= a.base;
        let firmware = [
            layout.pages,
            layout.data,
            layout.heap,
            layout.stack,
            layout.apob,
            layout.reset,
        ];
        for (i, a) in firmware.iter().enumerate() {
            assert!(a.size > 0);
            for b in &firmware[i + 1..] {
                assert!(disjoint(*a, *b));
            }
        }
        for r in [layout.pages, layout.data, layout.heap, layout.stack] {
            assert!(r.base >= layout.early.base && end(r) <= end(layout.early));
        }
        assert_eq!(end(layout.stack), end(layout.early));
        assert_eq!(layout.pages.size, (PDPT_COUNT + 1) * 0x1000);
        assert!(layout.cache.size.is_power_of_two());
        assert_eq!(layout.cache.base & (layout.cache.size - 1), 0);
        for r in [layout.early, layout.reset] {
            assert!(r.base >= layout.cache.base && end(r) <= end(layout.cache));
        }
        for r in [layout.payload_heap, layout.linux_params] {
            for f in firmware.into_iter().chain([layout.cache]) {
                assert!(disjoint(r, f));
            }
        }
    }

    #[test]
    fn console_kind_sets_the_legacy_device_flag() {
        struct Fch;
        impl Board for Fch {
            #[cfg(feature = "genoa")]
            type Soc = Genoa;
            #[cfg(all(feature = "siena", not(feature = "genoa")))]
            type Soc = Siena;
            const NAME: &'static str = "fch";
            const SMBIOS_MAX_DIMM_MIB: u32 = 1;
            const MEMORY_CHANNELS: u16 = 1;
            const FLASH_SIZE: u32 = 0;
            const CONSOLE: Console = Console::Fch(Uart::Uart1);
            const LINUX_BOOTARGS: &'static str = "";
        }
        assert!(!Fch::LEGACY_DEVICES);
        assert_eq!(Uart::Uart1.intr_source(), 0x75);
    }
}
