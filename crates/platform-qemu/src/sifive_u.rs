//! Direct QEMU `sifive_u` flow.
//!
//! QEMU enters the stage in live DRAM through `-bios`; unlike physical FU740
//! hardware this flow must not program PRCI or train DDR.

use serde::{Deserialize, Serialize};

pub const QEMU_SIFIVE_U_FFS_BASE: u64 = 0x8000_0000;
// Reservation for the memory-mapped boot image. This is a policy window,
// not the image size (mount uses the anchor's actual size). It must cover
// the largest packed image across payload selections while leaving the
// hardware-convention payload addresses (firmware 0x83000000, kernel
// 0x84000000, FDT workspace 0x90000000) outside it: loads into reserved
// windows are rejected by the boot policy.
pub const QEMU_SIFIVE_U_FFS_SIZE: u64 = 0x100_0000;
pub const QEMU_SIFIVE_U_RAM_SIZE: u64 = 0x4000_0000;
// FDT workspace must sit outside the reserved firmware window
// ([FFS_BASE, FFS_BASE + FFS_SIZE)) or workspace registration rejects it.
// 0x90000000 follows the 256 MiB firmware window inside DTB-discovered RAM.
pub const QEMU_SIFIVE_U_DTB_ADDR: u64 = 0x9000_0000;
pub const QEMU_SIFIVE_U_KERNEL_ADDR: u64 = 0x8400_0000;
pub const QEMU_SIFIVE_U_FIRMWARE_ADDR: u64 = 0x8300_0000;
pub const QEMU_SIFIVE_U_BOOTARGS: &str = "console=ttySIF0 earlycon=sbi";

/// Closed QEMU `sifive_u` policy consumed by its direct flow.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QemuSifiveUConfig {
    pub firmware_base: u64,
    pub firmware_size: u64,
    pub ram_base: u64,
    pub ram_size: u64,
    pub dtb_addr: u64,
    pub kernel_addr: u64,
    pub firmware_addr: u64,
    pub bootargs: &'static str,
}

impl QemuSifiveUConfig {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            firmware_base: QEMU_SIFIVE_U_FFS_BASE,
            firmware_size: QEMU_SIFIVE_U_FFS_SIZE,
            ram_base: QEMU_SIFIVE_U_FFS_BASE,
            ram_size: QEMU_SIFIVE_U_RAM_SIZE,
            dtb_addr: QEMU_SIFIVE_U_DTB_ADDR,
            kernel_addr: QEMU_SIFIVE_U_KERNEL_ADDR,
            firmware_addr: QEMU_SIFIVE_U_FIRMWARE_ADDR,
            bootargs: QEMU_SIFIVE_U_BOOTARGS,
        }
    }

    #[must_use]
    pub const fn build(self) -> Self {
        if self.firmware_size == 0
            || self.ram_size == 0
            || self.firmware_base.checked_add(self.firmware_size).is_none()
            || self.ram_base.checked_add(self.ram_size).is_none()
        {
            panic!("QEMU sifive_u firmware and RAM windows must be valid");
        }
        self
    }
}

impl Default for QemuSifiveUConfig {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(all(feature = "stage", feature = "riscv64", target_arch = "riscv64"))]
mod stage {
    use super::*;
    #[cfg(feature = "crabefi")]
    use fstart_core::services::Console;
    use fstart_core::services::ServiceError;
    use fstart_driver_uart::sifive::{SifiveUart, SifiveUartConfig};
    use fstart_payload::MainstagePayload;

    #[cfg(not(fstart_entry = "riscv64"))]
    compile_error!("sifive-u requires the riscv64 entry");
    #[cfg(any(
        not(any(
            fstart_payload = "halt",
            fstart_payload = "linux",
            fstart_payload = "crabefi"
        )),
        all(fstart_payload = "halt", fstart_payload = "linux"),
        all(fstart_payload = "halt", fstart_payload = "crabefi"),
        all(fstart_payload = "linux", fstart_payload = "crabefi")
    ))]
    compile_error!("select exactly one sifive-u payload");
    #[cfg(all(fstart_payload = "linux", not(feature = "linux")))]
    compile_error!("selected Linux backend is not enabled");
    #[cfg(all(fstart_payload = "crabefi", not(feature = "crabefi")))]
    compile_error!("selected CrabEFI backend is not enabled");

    /// DRAM-resident state retained through selected payload launch.
    pub struct QemuSifiveUMainstage {
        #[cfg(any(feature = "linux", feature = "crabefi"))]
        config: &'static QemuSifiveUConfig,
        console: SifiveUart,
    }

    impl QemuSifiveUMainstage {
        #[must_use]
        pub const fn console(&self) -> &SifiveUart {
            &self.console
        }
    }

    /// The QEMU `sifive_u` stage: console, boot media, then the payload.
    /// QEMU boards have no board work; their `main` calls this.
    pub fn run(config: &'static QemuSifiveUConfig, console: &'static SifiveUartConfig) -> ! {
        fstart_stage::anchor::initial_stage();
        let console_config = console;
        let mut console = match SifiveUart::new(console_config) {
            Ok(console) => console,
            Err(_) => fstart_arch::riscv64::halt(),
        };
        if console.init().is_err() {
            fstart_arch::riscv64::halt();
        }
        // SAFETY: the stage owns this console through payload handoff.
        unsafe { fstart_log::init(&console) };
        fstart_log::info!("sifive-u: SiFive UART console ready");
        if !phase(
            "boot_integrity",
            crate::boot::from_dtb(
                fstart_arch::riscv64::boot_dtb_addr(),
                cfg!(any(fstart_payload = "linux", fstart_payload = "crabefi"))
                    .then_some(config.dtb_addr),
                config.ram_base,
                config.firmware_base,
                config.firmware_size,
            ),
        ) {
            fstart_arch::riscv64::halt();
        }
        fstart_log::info!("sifive-u ramstage: ready for payload");
        let mainstage = QemuSifiveUMainstage {
            #[cfg(any(feature = "linux", feature = "crabefi"))]
            config,
            console,
        };
        mainstage.hand_off(|handoff| {
            <QemuSifiveUBuildSelectedPayload as MainstagePayload>::boot(handoff, ())
        })
    }

    /// Without CrabEFI nothing returns from OpenSBI here.
    #[cfg(not(feature = "crabefi"))]
    pub fn resume_sbi(
        _: &'static QemuSifiveUConfig,
        _: &'static SifiveUartConfig,
        _: u64,
        _: u64,
    ) -> ! {
        fstart_arch::riscv64::halt()
    }

    /// Recreate stage-local state after OpenSBI enters S-mode.
    #[cfg(feature = "crabefi")]
    pub fn resume_sbi(
        config: &'static QemuSifiveUConfig,
        console_config: &'static SifiveUartConfig,
        hart_id: u64,
        dtb_addr: u64,
    ) -> ! {
        let mut console = match SifiveUart::new(console_config) {
            Ok(console) => console,
            Err(_) => fstart_arch::riscv64::halt(),
        };
        if console.init().is_err() {
            fstart_arch::riscv64::halt();
        }
        // SAFETY: this resumed stage never returns; the console outlives CrabEFI.
        unsafe {
            fstart_log::replace_console(
                core::mem::transmute::<&dyn Console, &'static dyn Console>(&console),
            );
        }
        QemuSifiveUMainstage { config, console }.hand_off(|handoff| {
            fstart_payload::Riscv64UefiPayload::resume_sbi(handoff, hart_id, dtb_addr)
        })
    }

    fn phase(name: &str, result: Result<(), ServiceError>) -> bool {
        fstart_log::info!("sifive-u ramstage: {}", name);
        if result.is_err() {
            fstart_log::error!("sifive-u ramstage: {} failed", name);
            false
        } else {
            true
        }
    }

    impl QemuSifiveUMainstage {
        /// Describe the machine to `boot` and run it.
        fn hand_off(&self, boot: impl FnOnce(fstart_payload::Handoff<'_>) -> !) -> ! {
            use fstart_payload::{Description, Handoff, Placement};
            #[cfg(any(feature = "linux", feature = "crabefi"))]
            {
                use fstart_core::services::memory_detect::{E820Entry, E820Kind};
                use fstart_payload::Range;
                let config = self.config;
                let memory = [E820Entry::new(
                    config.ram_base,
                    config.ram_size,
                    E820Kind::Ram,
                )];
                let mut description = Description::new(
                    &memory,
                    Range::new(config.firmware_base, config.firmware_size),
                );
                description.console = Some(&self.console);
                description.device_tree = Some(fstart_arch::riscv64::boot_dtb_addr());
                // No described PCI root: CrabEFI inspects the firmware FDT
                // rather than invent bus bounds from a bare ECAM address.
                description.mmio = &crate::virt::QEMU_RISCV64_MMIO;
                let placement = Placement {
                    kernel: config.kernel_addr,
                    device_tree: config.dtb_addr,
                    firmware: Range::new(config.firmware_addr, 0x20_0000),
                    hart_id: fstart_arch::riscv64::boot_hart_id(),
                    bootargs: config.bootargs,
                    ..Default::default()
                };
                boot(Handoff::new(description, placement))
            }
            #[cfg(not(any(feature = "linux", feature = "crabefi")))]
            boot(Handoff::new(
                Description::new(&[], Default::default()),
                Placement::default(),
            ))
        }
    }

    pub type QemuSifiveUBuildSelectedPayload = fstart_payload::BuildSelectedPayload;
}

#[cfg(all(feature = "stage", feature = "riscv64", target_arch = "riscv64"))]
pub use stage::{QemuSifiveUBuildSelectedPayload, QemuSifiveUMainstage, resume_sbi, run};
