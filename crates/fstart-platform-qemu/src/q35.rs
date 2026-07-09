//! QEMU q35 host bridge setup.

use fstart_core::services::memory_detect::{E820Entry, E820Kind};
use fstart_core::services::ServiceError;
use fstart_pci::{
    PciBdf, PciEcam, PciEcamConfig, PciRootBus, PciWindow, PCI_HEADER_TYPE,
    PCI_HEADER_TYPE_MULTI_FUNC, PCI_INTERRUPT_LINE, PCI_INTERRUPT_PIN, PCI_VENDOR_ID,
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
        Ok(Self { config, ecam })
    }

    pub fn init_with_e820(&mut self, entries: &[E820Entry]) -> Result<(), ServiceError> {
        self.verify_machine_type()?;
        self.enable_ecam();
        self.program_pam();
        self.setup_ich9_pm_io();
        self.setup_legacy_pc_timers();

        let (tolud, touud) = Self::ram_tops_from_e820(entries);
        let ecam_end = self.config.ecam_base + self.config.ecam_size;
        let mmio32_base = tolud.max(ecam_end);
        let mmio32_size = MMIO32_LIMIT.saturating_sub(mmio32_base);
        let mmio64_base = touud;
        let mmio64_size = fstart_arch::x86::physical_address_limit().saturating_sub(mmio64_base);

        fstart_log::info!("Q35: TOLUD={:#x} TOUUD={:#x}", tolud, touud);
        fstart_log::info!("Q35: MMIO32={:#x}..{:#x}", mmio32_base, MMIO32_LIMIT);
        fstart_log::info!(
            "Q35: MMIO64={:#x}..{:#x}",
            mmio64_base,
            mmio64_base + mmio64_size,
        );

        self.ecam.configure_windows(
            mmio32_base,
            mmio32_size,
            mmio64_base,
            mmio64_size,
            PIO_BASE,
            PIO_SIZE,
        );
        self.ecam
            .enumerate_and_allocate()
            .map_err(|_| ServiceError::HardwareError)?;
        self.assign_irqs();
        Ok(())
    }

    pub fn device_count(&self) -> usize {
        self.ecam.device_count()
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
        fstart_log::info!(
            "Q35: PCIEXBAR enabled at {:#x} ({} buses)",
            self.config.ecam_base,
            bus_count,
        );
    }

    fn program_pam(&self) {
        let mch = PciBdf::new(0, 0, 0);
        let pam0 = self.ecam_read8(mch, PAM0);
        self.ecam_write8(mch, PAM0, pam0 | 0x30);
        for idx in 1u16..=6 {
            self.ecam_write8(mch, PAM0 + idx, 0x33);
        }
        fstart_log::info!("Q35: PAM0-6 programmed (legacy region -> DRAM)");
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

    fn ecam_read8(&self, addr: PciBdf, reg: u16) -> u8 {
        let val = self.ecam.config_read32(addr, reg & !0x3);
        let shift = ((reg & 0x3) * 8) as u32;
        ((val >> shift) & 0xff) as u8
    }

    fn ecam_read16(&self, addr: PciBdf, reg: u16) -> u16 {
        let val = self.ecam.config_read32(addr, reg & !0x3);
        let shift = ((reg & 0x2) * 8) as u32;
        ((val >> shift) & 0xffff) as u16
    }

    fn ecam_write8(&self, addr: PciBdf, reg: u16, val: u8) {
        let aligned = reg & !0x3;
        let shift = ((reg & 0x3) * 8) as u32;
        let mut dword = self.ecam.config_read32(addr, aligned);
        dword &= !(0xff << shift);
        dword |= (val as u32) << shift;
        self.ecam.config_write32(addr, aligned, dword);
    }

    fn assign_irqs(&self) {
        let bus = self.ecam.bus_start();
        for slot in 0u8..32 {
            let addr = PciBdf::new(bus, slot, 0);
            let vendor = self.ecam_read16(addr, PCI_VENDOR_ID);
            if vendor == 0xffff {
                continue;
            }
            let offset = if slot < 25 { slot as usize % 4 } else { 0 };
            let max_func = if self.is_multifunction(addr) { 8 } else { 1 };
            for func in 0..max_func {
                let faddr = PciBdf::new(bus, slot, func);
                if func > 0 && self.ecam_read16(faddr, PCI_VENDOR_ID) == 0xffff {
                    continue;
                }
                let pin = self.ecam_read8(faddr, PCI_INTERRUPT_PIN);
                if !(1..=4).contains(&pin) {
                    continue;
                }
                let irq = Q35_IRQS[(offset + pin as usize - 1) % Q35_IRQS.len()];
                self.ecam_write8(faddr, PCI_INTERRUPT_LINE, irq);
            }
        }
        fstart_log::info!("Q35: PCI IRQ routing assigned");
    }

    fn is_multifunction(&self, addr: PciBdf) -> bool {
        self.ecam_read8(addr, PCI_HEADER_TYPE) & PCI_HEADER_TYPE_MULTI_FUNC != 0
    }

    fn pci_write8(bus: u8, dev: u8, func: u8, reg: u8, val: u8) {
        let aligned = reg & !3;
        let shift = ((reg & 3) as u32) * 8;
        // SAFETY: caller selects Q35 PCI config registers.
        let old = unsafe { fstart_core::pio::pci_cfg_read32(bus, dev, func, aligned) };
        let new = (old & !(0xffu32 << shift)) | ((val as u32) << shift);
        // SAFETY: caller selects Q35 PCI config registers.
        unsafe { fstart_core::pio::pci_cfg_write32(bus, dev, func, aligned, new) };
    }

    fn pci_write_lpc32(reg: u8, val: u32) {
        // SAFETY: bus 0/device 31/function 0 is the ICH9 LPC bridge on Q35.
        unsafe { fstart_core::pio::pci_cfg_write32(0, ICH9_LPC_DEV, ICH9_LPC_FUNC, reg, val) }
    }

    fn pci_write_lpc8(reg: u8, val: u8) {
        Self::pci_write8(0, ICH9_LPC_DEV, ICH9_LPC_FUNC, reg, val);
    }

    fn setup_ich9_pm_io(&self) {
        Self::pci_write_lpc32(ICH9_PMBASE_REG, Q35_PMBASE as u32 | 1);
        Self::pci_write_lpc8(ICH9_ACPI_CNTL, 0x80);
        // SAFETY: Q35 PM timer lives at PMBASE+8 after programming above.
        let pmt = unsafe { fstart_core::pio::inl(Q35_PMBASE + 8) };
        fstart_log::info!("Q35: ICH9 PMBASE programmed");
        fstart_log::info!("Q35: ACPI PM timer initial value={:#x}", pmt);
    }

    fn setup_legacy_pc_timers(&self) {
        // SAFETY: standard PC-compatible PIC/PIT/CMOS ports on Q35.
        unsafe {
            fstart_core::pio::outb(0x20, 0x11);
            fstart_core::pio::outb(0xa0, 0x11);
            fstart_core::pio::outb(0x21, 0x20);
            fstart_core::pio::outb(0xa1, 0x28);
            fstart_core::pio::outb(0x21, 0x04);
            fstart_core::pio::outb(0xa1, 0x02);
            fstart_core::pio::outb(0x21, 0x01);
            fstart_core::pio::outb(0xa1, 0x01);
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

impl PciRootBus for Q35HostBridge {
    fn init_bus(&mut self) -> Result<(), ServiceError> {
        Ok(())
    }

    fn config_read32(&self, addr: PciBdf, reg: u16) -> Result<u32, ServiceError> {
        Ok(self.ecam.config_read32(addr, reg))
    }

    fn config_write32(&self, addr: PciBdf, reg: u16, val: u32) -> Result<(), ServiceError> {
        self.ecam.config_write32(addr, reg, val);
        Ok(())
    }

    fn ecam_base(&self) -> u64 {
        self.ecam.ecam_base()
    }

    fn ecam_size(&self) -> u64 {
        self.ecam.ecam_size()
    }

    fn bus_start(&self) -> u8 {
        self.ecam.bus_start()
    }

    fn bus_end(&self) -> u8 {
        self.ecam.bus_end()
    }

    fn device_count(&self) -> usize {
        self.ecam.device_count()
    }

    fn windows(&self) -> &[PciWindow] {
        self.ecam.windows()
    }
}
