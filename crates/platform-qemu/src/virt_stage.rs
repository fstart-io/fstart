//! The QEMU `virt` machines (RISC-V, AArch64, ARMv7) as blocks a board's
//! `main` calls in order:
//!
//! ```ignore
//! Virt::enter(&CONFIG, &CONSOLE, console()).boot_media().scan_pci().boot()
//! ```

use fstart_core::layout::{Layout, RegionKind};
use fstart_core::services::ConsoleDevice;
use fstart_core::services::memory_detect::{E820Entry, E820Kind};
use fstart_log::StaticConsole;
use fstart_payload::{Description, Handoff, MainstagePayload, Placement, Range};
use fstart_stage::{fail, step};

use crate::virt::{QemuVirtConfig, enumerate_pci};

/// A QEMU `virt` machine whose console is up.
pub struct Virt<C: 'static> {
    config: &'static QemuVirtConfig,
    layout: Layout<'static>,
    console: &'static C,
    /// The device tree QEMU passed.
    dtb: u64,
    /// The hart the stage runs on (RISC-V).
    hart: u64,
}

impl<C: ConsoleDevice + 'static> Virt<C> {
    /// Enter the stage and bring the console up in `slot`.
    pub fn enter(
        config: &'static QemuVirtConfig,
        slot: &'static StaticConsole<C>,
        console: C::Config,
    ) -> Self {
        fstart_stage::anchor::initial_stage();
        let Ok(layout) = fstart_stage::layout::current() else {
            fstart_arch::halt()
        };
        let console = Self::start_console(slot, console);
        fstart_log::info!("{}: {} console ready", config.name, C::NAME);
        let virt = Self {
            config,
            layout,
            console,
            dtb: source_dtb(config),
            #[cfg(target_arch = "riscv64")]
            hart: fstart_arch::riscv64::boot_hart_id(),
            #[cfg(not(target_arch = "riscv64"))]
            hart: 0,
        };
        fstart_log::info!(
            "resolved layout: stack={} heap={} firmware={}",
            virt.region(RegionKind::Stack).size,
            virt.region(RegionKind::Heap).size,
            virt.region(RegionKind::Firmware).size
        );
        virt
    }

    fn start_console(slot: &'static StaticConsole<C>, config: C::Config) -> &'static C {
        let Ok(mut console) = C::new(config) else {
            fstart_arch::halt()
        };
        if console.init().is_err() {
            fstart_arch::halt()
        }
        slot.install(console)
    }

    /// Find RAM in the boot device tree and mount the verified boot media.
    pub fn boot_media(self) -> Self {
        let firmware = self.region(RegionKind::Firmware);
        step("boot media", || {
            crate::boot::from_dtb_with_layout(
                self.dtb,
                self.layout.region(RegionKind::DeviceTree).map(|r| r.base),
                self.config.ram_base,
                firmware.base,
                firmware.size,
                Some(self.layout),
            )
        });
        self
    }

    /// Enumerate the ECAM root and assign its devices' resources.
    pub fn scan_pci(self) -> Self {
        let pci = step("PCI", || enumerate_pci(&self.config.pci));
        fstart_log::info!(
            "{}: PCI root ready ({} devices)",
            self.config.name,
            pci.device_count()
        );
        self
    }

    /// Describe the machine to the build's payload launcher and boot it.
    pub fn boot(self) -> ! {
        // AArch64 CrabEFI patches its device tree in the workspace.
        #[cfg(all(fstart_payload = "crabefi", target_arch = "aarch64"))]
        step("device tree", || {
            fstart_stage::copy_fdt_to_workspace(self.dtb, self.region(RegionKind::DeviceTree).base)
        });
        fstart_log::info!("{} ramstage: ready for payload", self.config.name);
        self.hand_off(|handoff| {
            <fstart_payload::BuildSelectedPayload as MainstagePayload>::boot(handoff, ())
        })
    }

    fn region(&self, kind: RegionKind) -> fstart_core::layout::Region {
        self.layout
            .region(kind)
            .unwrap_or_else(|| fail("stage layout"))
    }

    /// `kind`'s range, or empty when this build's layout has none.
    fn range(&self, kind: RegionKind) -> Range {
        self.layout
            .region(kind)
            .map_or(Range::default(), |r| Range::new(r.base, r.size))
    }

    fn hand_off(&self, boot: impl FnOnce(Handoff<'_>) -> !) -> ! {
        let config = self.config;
        let ram = fstart_stage::directory::writable_memory()
            .and_then(|windows| windows.iter().find(|w| w.start == config.ram_base))
            .map_or(0, |window| window.size);
        let memory = [E820Entry::new(config.ram_base, ram, E820Kind::Ram)];
        let image = fstart_core::services::ffs_context::memory_mapped()
            .unwrap_or_else(|| fail("boot media"));
        let mut description =
            Description::new(&memory, Range::new(image.image_base, image.image_size));
        description.console = Some(self.console);
        description.device_tree = Some(self.dtb);
        // The AArch64 stage runs from RAM, so its UEFI payload keeps the
        // flash out of its allocations. RISC-V executes from that flash.
        #[cfg(target_arch = "aarch64")]
        let reserved = [self.range(RegionKind::Flash)];
        #[cfg(target_arch = "aarch64")]
        {
            description.reserved = &reserved;
        }
        description.mmio = config.mmio;
        description.pci_root = Some(fstart_pci::PciRootProvider::root_info(&config.pci));
        let placement = Placement {
            kernel: self.range(RegionKind::Payload).base,
            device_tree: self.range(RegionKind::DeviceTree).base,
            firmware: self.range(RegionKind::PayloadFirmware),
            hart_id: self.hart,
            bootargs: config.bootargs,
            ..Default::default()
        };
        boot(Handoff::new(description, placement))
    }
}

/// The device tree QEMU passed: the boot register, else where QEMU leaves it
/// for firmware boots.
fn source_dtb(config: &QemuVirtConfig) -> u64 {
    #[cfg(target_arch = "aarch64")]
    let boot = fstart_arch::aarch64::boot_dtb_addr();
    #[cfg(target_arch = "riscv64")]
    let boot = fstart_arch::riscv64::boot_dtb_addr();
    #[cfg(not(any(target_arch = "aarch64", target_arch = "riscv64")))]
    let boot = 0;
    if boot != 0 { boot } else { config.dtb_fallback }
}

/// OpenSBI's return into S-mode, before CrabEFI.
#[cfg(all(target_arch = "riscv64", feature = "crabefi"))]
impl<C: ConsoleDevice + 'static> Virt<C> {
    /// Recreate the stage's state after OpenSBI enters S-mode and launch
    /// CrabEFI. The image stays mapped across the transition.
    pub fn resume_sbi(
        config: &'static QemuVirtConfig,
        slot: &'static StaticConsole<C>,
        console: C::Config,
        hart_id: u64,
        dtb_addr: u64,
    ) -> ! {
        let Ok(layout) = fstart_stage::layout::current() else {
            fstart_arch::halt()
        };
        let console = match slot.get() {
            Some(console) => console,
            None => Self::start_console(slot, console),
        };
        // SAFETY: the console is a static.
        unsafe { fstart_log::replace_console(console) };
        fstart_log::info!("riscv64 uefi: OpenSBI returned, launching CrabEFI");
        // S-mode cannot read the M-mode CSRs the boot values were kept in;
        // OpenSBI passes both.
        Self {
            config,
            layout,
            console,
            dtb: dtb_addr,
            hart: hart_id,
        }
        .hand_off(|handoff| {
            fstart_payload::Riscv64UefiPayload::resume_sbi(handoff, hart_id, dtb_addr)
        })
    }
}

/// Without CrabEFI nothing returns from OpenSBI here.
#[cfg(all(target_arch = "riscv64", not(feature = "crabefi")))]
impl<C: ConsoleDevice + 'static> Virt<C> {
    pub fn resume_sbi(
        _: &'static QemuVirtConfig,
        _: &'static StaticConsole<C>,
        _: C::Config,
        _: u64,
        _: u64,
    ) -> ! {
        fstart_arch::halt()
    }
}
