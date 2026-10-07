//! QEMU q35 host bridge setup.

use fstart_core::services::ServiceError;
use fstart_core::services::memory_detect::{E820Entry, E820Kind};
use fstart_pci::{
    PCI_HEADER_TYPE, PCI_HEADER_TYPE_MULTI_FUNC, PCI_INTERRUPT_LINE, PCI_INTERRUPT_PIN, PciAddress,
    PciEcam, PciEcamConfig,
};
use serde::{Deserialize, Serialize};

const MMIO32_LIMIT: u64 = 0xFE00_0000;
const PAM0: u16 = 0x90;
const Q35_IRQS: [u8; 8] = [10, 10, 11, 11, 10, 10, 11, 11];
const Q35_MCH_VID: u16 = 0x8086;
const Q35_MCH_DID: u16 = 0x29c0;
const PIO_BASE: u64 = 0x1000;
const PIO_SIZE: u64 = 0xf000;

const ICH9_LPC_DEV: u8 = 31;
const ICH9_LPC_FUNC: u8 = 0;
const ICH9_PMBASE_REG: u8 = 0x40;
const ICH9_ACPI_CNTL: u8 = 0x44;
const Q35_PMBASE: u16 = 0x0600;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Q35HostBridgeConfig {
    pub ecam_base: u64,
    pub ecam_size: u64,
    pub bus_start: u8,
    pub bus_end: u8,
}

pub struct Q35HostBridge {
    config: Q35HostBridgeConfig,
    ecam: PciEcam,
    /// TSEG base captured from the e820 map during [`Self::init_with_e820`].
    /// Zero until initialized; consumed by the SMM flow (`q35_smm`).
    #[cfg(all(feature = "smm", feature = "stage"))]
    tseg_base: u64,
}

unsafe impl Send for Q35HostBridge {}
unsafe impl Sync for Q35HostBridge {}

impl Q35HostBridge {
    pub fn new(config: Q35HostBridgeConfig) -> Result<Self, ServiceError> {
        let ecam = PciEcam::from_config(&PciEcamConfig {
            ecam_base: config.ecam_base,
            ecam_size: config.ecam_size,
            mmio32_base: 0,
            mmio32_size: 0,
            mmio64_base: 0,
            mmio64_size: 0,
            pio_base: 0,
            pio_size: 0,
            bus_start: config.bus_start,
            bus_end: config.bus_end,
        })
        .map_err(|_| ServiceError::InvalidParam)?;
        Ok(Self {
            config,
            ecam,
            #[cfg(all(feature = "smm", feature = "stage"))]
            tseg_base: 0,
        })
    }

    pub fn init_with_e820(&mut self, entries: &[E820Entry]) -> Result<(), ServiceError> {
        self.enable_config_access()?;
        self.program_pam()?;
        self.setup_ich9_pm_io()?;
        self.setup_legacy_pc_timers();

        let (tolud, touud) = Self::ram_tops_from_e820(entries);
        let ecam_end = self.config.ecam_base + self.config.ecam_size;
        let mmio32_base = tolud.max(ecam_end);
        let mmio32_size = MMIO32_LIMIT.saturating_sub(mmio32_base);
        let mmio64_base = touud;
        let mmio64_size = 0x0010_0000_0000_0000u64.saturating_sub(mmio64_base);

        fstart_log::info!("Q35: TOLUD={:#x} TOUUD={:#x}", tolud, touud);
        fstart_log::info!("Q35: MMIO32={:#x}..{:#x}", mmio32_base, MMIO32_LIMIT);
        fstart_log::info!(
            "Q35: MMIO64={:#x}..{:#x}",
            mmio64_base,
            mmio64_base + mmio64_size,
        );

        self.ecam
            .configure_windows(
                mmio32_base,
                mmio32_size,
                mmio64_base,
                mmio64_size,
                PIO_BASE,
                PIO_SIZE,
            )
            .map_err(|_| ServiceError::HardwareError)?;
        self.ecam
            .enumerate_and_allocate()
            .map_err(|_| ServiceError::HardwareError)?;
        self.assign_irqs()?;
        // Capture the TSEG window for the SMM flow while the firmware map is
        // at hand. ECAM is enabled before decoding the MCH geometry.
        #[cfg(all(feature = "smm", feature = "stage"))]
        {
            let size = crate::q35_smm::decode_tseg_size();
            self.tseg_base = crate::q35_smm::tseg_base_from_e820(entries, size);
            fstart_log::info!(
                "Q35: TSEG window base={:#x} size={:#x}",
                self.tseg_base,
                size
            );
        }
        Ok(())
    }

    pub fn device_count(&self) -> usize {
        self.ecam.device_count()
    }

    /// Config-space access for post-enumeration PCI children
    /// (e.g. the bochs-display probe after `init_with_e820`).
    pub fn ecam(&self) -> &PciEcam {
        &self.ecam
    }

    /// TSEG base captured during [`Self::init_with_e820`]; zero before init.
    #[cfg(all(feature = "smm", feature = "stage"))]
    pub(crate) fn tseg_base(&self) -> u64 {
        self.tseg_base
    }

    /// Make configuration space available before TSEG discovery or PCI setup.
    /// This does not allocate resources or change the memory map.
    pub fn enable_config_access(&self) -> Result<(), ServiceError> {
        self.verify_machine_type()?;
        self.enable_ecam();
        Ok(())
    }

    fn enable_ecam(&self) {
        const PCIEXBAR_LO: u8 = 0x60;
        const PCIEXBAR_HI: u8 = 0x64;
        let bus_count = self.config.bus_end as u32 - self.config.bus_start as u32 + 1;
        let length_bits = match bus_count {
            256 => 0 << 1,
            128 => 1 << 1,
            64 => 2 << 1,
            _ => 0 << 1,
        };
        let pciexbar = (self.config.ecam_base as u32) | length_bits | 1;
        // SAFETY: Q35 MCH is bus 0 device 0 function 0; CF8/CFC are x86 PCI config ports.
        unsafe {
            fstart_core::pio::pci_cfg_write32(0, 0, 0, PCIEXBAR_HI, 0);
            fstart_core::pio::pci_cfg_write32(0, 0, 0, PCIEXBAR_LO, pciexbar);
        }
        fstart_pci::ecam::init(self.config.ecam_base as usize);
        fstart_log::info!(
            "Q35: PCIEXBAR enabled at {:#x} ({} buses)",
            self.config.ecam_base,
            bus_count,
        );
    }

    fn program_pam(&self) -> Result<(), ServiceError> {
        let mch = self
            .ecam
            .device(PciAddress::new(0, 0, 0, 0))
            .ok_or(ServiceError::InvalidParam)?;
        mch.or8(PAM0, 0x30);
        for idx in 1u16..=6 {
            mch.write8(PAM0 + idx, 0x33);
        }
        fstart_log::info!("Q35: PAM0-6 programmed (legacy region -> DRAM)");
        Ok(())
    }

    fn verify_machine_type(&self) -> Result<(), ServiceError> {
        // SAFETY: CF8/CFC are standard x86 PCI config I/O ports.
        let id = unsafe { fstart_core::pio::pci_cfg_read32(0, 0, 0, 0) };
        let vendor = (id & 0xffff) as u16;
        let device = ((id >> 16) & 0xffff) as u16;
        if vendor != Q35_MCH_VID || device != Q35_MCH_DID {
            fstart_log::error!(
                "Q35: unexpected MCH at 00:00.0: {:#06x}:{:#06x}",
                vendor,
                device,
            );
            return Err(ServiceError::HardwareError);
        }
        fstart_log::info!("Q35: MCH verified ({:#06x}:{:#06x})", vendor, device);
        Ok(())
    }

    fn ram_tops_from_e820(entries: &[E820Entry]) -> (u64, u64) {
        let mut tolud = 0u64;
        let mut touud = 0x1_0000_0000u64;
        for entry in entries {
            let kind = entry.kind;
            if kind != E820Kind::Ram as u32 {
                continue;
            }
            let top = entry.addr.saturating_add(entry.size);
            if top <= 0x1_0000_0000 && top > tolud {
                tolud = top;
            }
            if top > touud {
                touud = top;
            }
        }
        (tolud, touud)
    }

    fn assign_irqs(&self) -> Result<(), ServiceError> {
        let bus = self.ecam.bus_start();
        for slot in 0u8..32 {
            let addr = PciAddress::new(0, bus, slot, 0);
            let device = self.ecam.device(addr).ok_or(ServiceError::InvalidParam)?;
            if !device.is_present() {
                continue;
            }
            let offset = if slot < 25 { slot as usize % 4 } else { 0 };
            let max_func = if device.read8(PCI_HEADER_TYPE) & PCI_HEADER_TYPE_MULTI_FUNC != 0 {
                8
            } else {
                1
            };
            for func in 0..max_func {
                let faddr = PciAddress::new(0, bus, slot, func);
                let function = self.ecam.device(faddr).ok_or(ServiceError::InvalidParam)?;
                if func > 0 && !function.is_present() {
                    continue;
                }
                let pin = function.read8(PCI_INTERRUPT_PIN);
                if !(1..=4).contains(&pin) {
                    continue;
                }
                let irq = Q35_IRQS[(offset + pin as usize - 1) % Q35_IRQS.len()];
                function.write8(PCI_INTERRUPT_LINE, irq);
            }
        }
        fstart_log::info!("Q35: PCI IRQ routing assigned");
        Ok(())
    }

    fn setup_ich9_pm_io(&self) -> Result<(), ServiceError> {
        let lpc = self
            .ecam
            .device(PciAddress::new(0, 0, ICH9_LPC_DEV, ICH9_LPC_FUNC))
            .ok_or(ServiceError::InvalidParam)?;
        lpc.write32(ICH9_PMBASE_REG as u16, Q35_PMBASE as u32 | 1);
        lpc.write8(ICH9_ACPI_CNTL as u16, 0x80);
        // SAFETY: Q35 PM timer lives at PMBASE+8 after programming above.
        let pmt = unsafe { fstart_core::pio::inl(Q35_PMBASE + 8) };
        fstart_log::info!("Q35: ICH9 PMBASE programmed");
        fstart_log::info!("Q35: ACPI PM timer initial value={:#x}", pmt);
        Ok(())
    }

    fn setup_legacy_pc_timers(&self) {
        // SAFETY: standard PC-compatible PIC/PIT/CMOS ports on Q35.
        unsafe {
            fstart_arch::x86::legacy_pc::initialize_pic(0x20, 0x28);
            fstart_core::pio::outb(0xa1, 0xff);
            fstart_core::pio::outb(0x21, 0xfb);

            fstart_core::pio::outb(0x43, 0x36);
            fstart_core::pio::outb(0x40, 0x00);
            fstart_core::pio::outb(0x40, 0x00);
            fstart_core::pio::outb(0x43, 0x56);
            fstart_core::pio::outb(0x41, 0x12);

            let port61 = (fstart_core::pio::inb(0x61) & 0x0f) & !0x08 | 0x04;
            fstart_core::pio::outb(0x61, port61);
            let nmi = fstart_core::pio::inb(0x70) | 0x80;
            fstart_core::pio::outb(0x70, nmi);
        }
        fstart_log::info!("Q35: legacy 8259 PIC and 8254 PIT initialized");
    }
}
