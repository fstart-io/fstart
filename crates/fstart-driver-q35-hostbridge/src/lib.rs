//! Q35 PCI host bridge driver.
//!
//! This driver handles the Intel Q35 chipset's PCI root complex as
//! emulated by QEMU.  It differs from the generic [`PciEcam`] driver in
//! two ways:
//!
//! 1. **ECAM bootstrap via CF8/CFC** — on x86, the ECAM (PCIEXBAR) is
//!    not active at reset and must be programmed through legacy I/O port
//!    config access before memory-mapped config space works.
//!
//! 2. **Runtime MMIO window computation** — the 32-bit PCI MMIO hole
//!    depends on the amount of installed DRAM.  The MMIO32 window starts
//!    at `max(TOLUD, ecam_end)` and runs up to 0xFE00_0000 (below
//!    IOAPIC / LAPIC / ROM).  TOLUD is derived from the e820 memory map
//!    that `MemoryDetect` has already populated.
//!
//! The driver composes a [`PciEcam`] internally and delegates bus
//! enumeration, BAR allocation, and `PciRootBus` queries to it.
//!
//! Compatible: `"q35-hostbridge"`.

#![no_std]

extern crate alloc;

use core::cell::UnsafeCell;

use fstart_driver_pci_ecam::{PciEcam, PciEcamConfig};
use fstart_mp::{SmmError, SmmInfo, SmmOps};
use fstart_services::device::{Device, DeviceError};
use fstart_services::memory_detect::E820Entry;
use fstart_services::pci::{
    PciAddr, PciRootBus, PciWindow, PCI_HEADER_TYPE, PCI_HEADER_TYPE_MULTI_FUNC,
    PCI_INTERRUPT_LINE, PCI_INTERRUPT_PIN, PCI_VENDOR_ID,
};
use fstart_services::ServiceError;
use serde::{Deserialize, Serialize};

/// Upper bound of the 32-bit PCI MMIO window (exclusive).
///
/// Below this are the IOAPIC (0xFEC0_0000), LAPIC (0xFEE0_0000), and
/// the flash/ROM region.  Matches coreboot's `DOMAIN_RESOURCE_32BIT_LIMIT`.
const MMIO32_LIMIT: u64 = 0xFE00_0000;

// -----------------------------------------------------------------------
// Q35 MCH (bus 0, dev 0, fn 0) register offsets
// -----------------------------------------------------------------------

/// PAM registers: Programmable Attribute Map.
///
/// 7 registers (PAM0..PAM6) control access routing for the legacy
/// 0xC0000-0xFFFFF region.  Each register has two 4-bit nibbles
/// controlling two 16 KiB sub-regions.  Value `0x3` per nibble =
/// full DRAM read+write.
const PAM0: u16 = 0x90;

// -----------------------------------------------------------------------
// PCI IRQ routing table — matches coreboot qemu-q35 mainboard.c
// -----------------------------------------------------------------------

/// IRQ rotation table for PCI slots.
///
/// Uses legacy PIC IRQs 10 and 11 only.  Each PCI device has 4 interrupt
/// pins (INTA-INTD); the slot number mod 4 selects a starting offset
/// into this table.
const Q35_IRQS: [u8; 8] = [10, 10, 11, 11, 10, 10, 11, 11];

/// Default 64-bit MMIO limit: 39-bit physical = 512 GiB.
/// Used as a conservative fallback when CPUID detection fails.
const MMIO64_LIMIT_DEFAULT: u64 = 0x80_0000_0000;

/// Expected Q35 MCH (host bridge) PCI vendor/device ID.
/// Intel 82G33/G31/P35/P31 Express DRAM Controller (device 29c0).
const Q35_MCH_VID: u16 = 0x8086;
const Q35_MCH_DID: u16 = 0x29C0;

/// Full x86 I/O port range: 0x0000..0xFFFF (64 KiB).
/// The PCI root bridge decodes the entire I/O space; legacy ISA
/// devices below 0x1000 are handled by subtractive decode.
const PIO_BASE: u64 = 0x0000;
const PIO_SIZE: u64 = 0x10000;

// Q35 / ICH9 SMM registers.  Matches coreboot's
// `mainboard/emulation/qemu-q35/q35.h` and `i82801ix.h`.
const EXT_TSEG_MBYTES: u8 = 0x50;
const SMRAMC: u8 = 0x9d;
const G_SMRAME: u8 = 1 << 3;
const D_LCK: u8 = 1 << 4;
const D_OPEN: u8 = 1 << 6;
const C_BASE_SEG: u8 = 0b010;
const ESMRAMC: u8 = 0x9e;
const T_EN: u8 = 1 << 0;
const TSEG_SZ_MASK: u8 = 3 << 1;
const ICH9_LPC_DEV: u8 = 31;
const ICH9_LPC_FUNC: u8 = 0;
const ICH9_PMBASE_REG: u8 = 0x40;
const ICH9_ACPI_CNTL: u8 = 0x44;
const Q35_PMBASE: u16 = 0x0600;
const APM_CNT: u16 = 0x00b2;
const APM_CNT_SMI: u8 = 0xef;
const SMM_DEFAULT_SMBASE: u64 = 0x30000;
const AMD64_SAVE_STATE_SIZE: usize = 0x400;
const AMD64_SMBASE_SAVE_STATE_OFFSET: u16 = 0xff00;

const ZERO_CPU_LAYOUT: fstart_smm::CpuSmmLayout = fstart_smm::CpuSmmLayout {
    smbase: 0,
    entry_addr: 0,
    save_state_base: 0,
    save_state_top: 0,
    stack_bottom: 0,
    stack_top: 0,
};

struct CpuLayoutStore(UnsafeCell<[fstart_smm::CpuSmmLayout; fstart_smm::runtime::MAX_SMM_CPUS]>);
struct SmbaseStore(UnsafeCell<[u64; fstart_smm::runtime::MAX_SMM_CPUS]>);

// SAFETY: firmware runs the SMM installer from the BSP while SMRAM is open;
// no other Rust code accesses these scratch buffers concurrently.
unsafe impl Sync for CpuLayoutStore {}
unsafe impl Sync for SmbaseStore {}

static Q35_SMM_CPU_LAYOUTS: CpuLayoutStore = CpuLayoutStore(UnsafeCell::new(
    [ZERO_CPU_LAYOUT; fstart_smm::runtime::MAX_SMM_CPUS],
));
static Q35_SMM_RELOCATION_SMBASES: SmbaseStore =
    SmbaseStore(UnsafeCell::new([0; fstart_smm::runtime::MAX_SMM_CPUS]));

// -----------------------------------------------------------------------
// Config
// -----------------------------------------------------------------------

/// Typed configuration for the Q35 host bridge.
///
/// Only the ECAM base/size and bus range are needed.  MMIO windows are
/// computed at runtime from the e820 memory map.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Q35HostBridgeConfig {
    /// ECAM base address (PCIEXBAR).  Typically 0xB000_0000 on Q35.
    pub ecam_base: u64,
    /// ECAM region size in bytes (256 MiB for 256 buses).
    pub ecam_size: u64,
    /// First bus number in this segment.
    pub bus_start: u8,
    /// Last bus number in this segment.
    pub bus_end: u8,
}

// -----------------------------------------------------------------------
// Driver
// -----------------------------------------------------------------------

/// Q35 PCI host bridge driver.
///
/// Wraps a [`PciEcam`] with x86-specific ECAM bootstrap and
/// runtime MMIO window computation.
pub struct Q35HostBridge {
    config: Q35HostBridgeConfig,
    ecam: PciEcam,
}

// SAFETY: Same as PciEcam — hardware-fixed MMIO addresses,
// single-threaded firmware init.
unsafe impl Send for Q35HostBridge {}
unsafe impl Sync for Q35HostBridge {}

impl Q35HostBridge {
    /// Program the MCH's PCIEXBAR register via legacy CF8/CFC I/O ports.
    ///
    /// After this call, ECAM is live and all PCI config access goes
    /// through memory-mapped MMIO.
    fn enable_ecam(&self) {
        const PCIEXBAR_LO: u8 = 0x60;
        const PCIEXBAR_HI: u8 = 0x64;

        let bus_count = (self.config.bus_end as u32) - (self.config.bus_start as u32) + 1;
        let length_bits: u32 = match bus_count {
            256 => 0 << 1, // 256 MiB
            128 => 1 << 1, // 128 MiB
            64 => 2 << 1,  // 64 MiB
            _ => 0 << 1,   // default to 256
        };

        let pciexbar_val = (self.config.ecam_base as u32) | length_bits | 1;

        // SAFETY: MCH is at bus 0, dev 0, fn 0.  CF8/CFC are standard
        // x86 PCI config I/O ports.
        unsafe {
            fstart_pio::pci_cfg_write32(0, 0, 0, PCIEXBAR_HI, 0);
            fstart_pio::pci_cfg_write32(0, 0, 0, PCIEXBAR_LO, pciexbar_val);
        }

        fstart_log::info!(
            "Q35: PCIEXBAR enabled at {:#x} ({} buses)",
            self.config.ecam_base,
            bus_count
        );
    }

    /// Open the legacy 0xC0000-0xFFFFF region for DRAM read/write.
    ///
    /// PAM0 controls 0xF0000-0xFFFFF (the BIOS area) — read-modify-write
    /// to preserve the lower nibble.  PAM1-6 each control two 16 KiB
    /// sub-regions; `0x33` enables DRAM R/W for both.
    ///
    /// Matches coreboot `qemu_nb_init()` in `qemu-q35/mainboard.c`.
    fn program_pam(&self) {
        let mch = PciAddr::new(0, 0, 0);

        // PAM0: preserve lower nibble, set upper nibble to 0x3 (DRAM R/W
        // for 0xF0000-0xFFFFF).
        let pam0 = self.ecam.config_read8(mch, PAM0).unwrap_or(0);
        let _ = self.ecam.config_write8(mch, PAM0, pam0 | 0x30);

        // PAM1-PAM6: full DRAM access for 0xC0000-0xEFFFF.
        for i in 1u16..=6 {
            let _ = self.ecam.config_write8(mch, PAM0 + i, 0x33);
        }

        fstart_log::info!("Q35: PAM0-6 programmed (legacy region -> DRAM)");
    }

    /// Assign PCI interrupt lines to all discovered devices.
    ///
    /// Follows coreboot's Q35 IRQ assignment pattern:
    /// - Slots 0-24: IRQ table offset by `slot % 4` (standard swizzle)
    /// - Slots 25-31: IRQ table at offset 0 (southbridge devices)
    ///
    /// For each device that has an interrupt pin configured, writes the
    /// IRQ number to `PCI_INTERRUPT_LINE` (config reg 0x3C).
    fn assign_irqs(&self) {
        let bus = self.ecam.bus_start();
        for slot in 0u8..32 {
            // Check if a device exists at this slot (function 0).
            let addr = PciAddr::new(bus, slot, 0);
            let vendor = self
                .ecam
                .config_read16(addr, PCI_VENDOR_ID)
                .unwrap_or(0xFFFF);
            if vendor == 0xFFFF {
                continue;
            }

            let offset = if slot < 25 { (slot as usize) % 4 } else { 0 };

            // Assign IRQs for all functions of this device.
            let max_func = if self.is_multifunction(addr) { 8 } else { 1 };
            for func in 0..max_func {
                let faddr = PciAddr::new(bus, slot, func);
                if func > 0 {
                    let fv = self
                        .ecam
                        .config_read16(faddr, PCI_VENDOR_ID)
                        .unwrap_or(0xFFFF);
                    if fv == 0xFFFF {
                        continue;
                    }
                }
                let pin = self
                    .ecam
                    .config_read8(faddr, PCI_INTERRUPT_PIN)
                    .unwrap_or(0);
                if pin == 0 || pin > 4 {
                    continue; // no interrupt pin
                }
                // Pin 1=INTA..4=INTD, index into rotated table.
                let irq_idx = (offset + (pin as usize) - 1) % Q35_IRQS.len();
                let irq = Q35_IRQS[irq_idx];
                let _ = self.ecam.config_write8(faddr, PCI_INTERRUPT_LINE, irq);
            }
        }

        fstart_log::info!("Q35: PCI IRQ routing assigned");
    }

    /// Check if device at `addr` is multi-function (bit 7 of header type).
    fn is_multifunction(&self, addr: PciAddr) -> bool {
        let hdr = self.ecam.config_read8(addr, PCI_HEADER_TYPE).unwrap_or(0);
        hdr & PCI_HEADER_TYPE_MULTI_FUNC != 0
    }

    /// Compute TOLUD and TOUUD from e820 entries.
    ///
    /// - TOLUD: highest RAM end below 4 GiB.
    /// - TOUUD: highest RAM end (any address).  When there is no RAM
    ///   above 4 GiB, TOUUD equals 4 GiB so the MMIO64 window starts
    ///   immediately at the top of the 32-bit space.
    fn ram_tops_from_e820(entries: &[E820Entry]) -> (u64, u64) {
        let mut tolud: u64 = 0;
        let mut touud: u64 = 0x1_0000_0000; // default: 4 GiB
        for e in entries {
            // kind == 1 is E820Kind::Ram
            if e.kind == 1 {
                let top = e.addr.saturating_add(e.size);
                if top <= 0x1_0000_0000 && top > tolud {
                    tolud = top;
                }
                if top > touud {
                    touud = top;
                }
            }
        }
        (tolud, touud)
    }

    /// Read the CPU's physical address width from CPUID leaf 0x80000008.
    ///
    /// Returns the MMIO64 limit (1 << phys_bits). Falls back to
    /// `MMIO64_LIMIT_DEFAULT` (39-bit / 512 GiB) if the leaf is
    /// unavailable.
    fn detect_phys_addr_limit() -> u64 {
        // SAFETY: CPUID is always available on x86_64.
        let (max_ext_leaf, _, _, _) = unsafe { Self::cpuid(0x80000000) };
        if max_ext_leaf < 0x80000008 {
            fstart_log::info!(
                "Q35: CPUID 0x80000008 unavailable, using {}-bit default",
                MMIO64_LIMIT_DEFAULT.trailing_zeros()
            );
            return MMIO64_LIMIT_DEFAULT;
        }
        let (eax, _, _, _) = unsafe { Self::cpuid(0x80000008) };
        let phys_bits = (eax & 0xFF) as u32;
        let limit = if phys_bits >= 64 {
            u64::MAX
        } else {
            1u64 << phys_bits
        };
        fstart_log::info!("Q35: physical address width: {} bits", phys_bits);
        limit
    }

    /// Execute CPUID instruction.
    ///
    /// # Safety
    /// Only valid on x86/x86_64 targets.
    #[inline]
    unsafe fn cpuid(leaf: u32) -> (u32, u32, u32, u32) {
        let (eax, ebx, ecx, edx): (u32, u32, u32, u32);
        unsafe {
            core::arch::asm!(
                "push rbx",
                "cpuid",
                "mov {ebx_out:e}, ebx",
                "pop rbx",
                in("eax") leaf,
                in("ecx") 0u32,
                ebx_out = out(reg) ebx,
                lateout("eax") eax,
                lateout("ecx") ecx,
                lateout("edx") edx,
                options(nomem),
            );
        }
        (eax, ebx, ecx, edx)
    }

    /// Verify that the MCH at bus 0, dev 0, fn 0 is the expected Q35 device.
    ///
    /// Reads the PCI vendor/device ID via legacy CF8/CFC before ECAM is
    /// active. Matches coreboot's `mainboard_machine_check()` pattern.
    fn verify_machine_type(&self) -> Result<(), DeviceError> {
        // SAFETY: CF8/CFC is the standard x86 PCI config I/O port pair.
        let vid = unsafe { fstart_pio::pci_cfg_read32(0, 0, 0, 0) };
        let vendor = (vid & 0xFFFF) as u16;
        let device = ((vid >> 16) & 0xFFFF) as u16;
        if vendor != Q35_MCH_VID || device != Q35_MCH_DID {
            fstart_log::error!(
                "Q35: unexpected MCH at 00:00.0: vendor={:#06x} device={:#06x} \
                 (expected {:#06x}:{:#06x})",
                vendor,
                device,
                Q35_MCH_VID,
                Q35_MCH_DID
            );
            return Err(DeviceError::InitFailed);
        }
        fstart_log::info!("Q35: MCH verified ({:#06x}:{:#06x})", vendor, device);
        Ok(())
    }

    /// Configure MMIO windows from the e820 memory map and call
    /// `init()` on the inner PCI ECAM driver.
    ///
    /// This is the main entry point.  The codegen calls this instead of
    /// the `Device::init()` trait method, passing the e820 data that
    /// `MemoryDetect` has already populated.
    ///
    /// Window computation follows coreboot's Q35/i440fx pattern:
    /// - **MMIO32**: `max(TOLUD, ecam_end)` up to `0xFE00_0000`
    /// - **MMIO64**: starts above TOUUD (top of all RAM), extends to
    ///   the CPU's physical address limit (from CPUID 0x80000008)
    /// - **I/O**: full 64 KiB port space (`0x0000..0xFFFF`)
    pub fn init_with_e820(&mut self, entries: &[E820Entry]) -> Result<(), DeviceError> {
        // Step 0: Verify we're running on Q35 hardware (read MCH PCI ID).
        self.verify_machine_type()?;

        // Step 1: Enable ECAM via CF8/CFC.
        self.enable_ecam();

        // Step 2: Open legacy region (0xC0000-0xFFFFF) for DRAM access.
        self.program_pam();

        // Step 3: Compute MMIO windows from memory layout.
        let (tolud, touud) = Self::ram_tops_from_e820(entries);
        let ecam_end = self.config.ecam_base + self.config.ecam_size;

        // MMIO32 starts after both DRAM and ECAM.
        let mmio32_base = core::cmp::max(tolud, ecam_end);
        let mmio32_size = MMIO32_LIMIT.saturating_sub(mmio32_base);

        // MMIO64 starts above all RAM (including high RAM above 4 GiB).
        // Use CPUID to determine the CPU's actual physical address width.
        let mmio64_limit = Self::detect_phys_addr_limit();
        let mmio64_base = touud;
        let mmio64_size = mmio64_limit.saturating_sub(mmio64_base);

        fstart_log::info!("Q35: TOLUD={:#x} TOUUD={:#x}", tolud, touud);
        fstart_log::info!("Q35: MMIO32={:#x}..{:#x}", mmio32_base, MMIO32_LIMIT);
        fstart_log::info!(
            "Q35: MMIO64={:#x}..{:#x}",
            mmio64_base,
            mmio64_base + mmio64_size
        );

        self.ecam.configure_windows(
            mmio32_base,
            mmio32_size,
            mmio64_base,
            mmio64_size,
            PIO_BASE,
            PIO_SIZE,
        );

        // Step 4: Enumerate and allocate BARs (delegated to PciEcam).
        self.ecam.init()?;

        // Step 5: Assign PCI IRQ routing to all discovered devices.
        self.assign_irqs();

        Ok(())
    }
}

impl Q35HostBridge {
    fn pci_read8(bus: u8, dev: u8, func: u8, reg: u8) -> u8 {
        let aligned = reg & !3;
        let shift = ((reg & 3) as u32) * 8;
        // SAFETY: caller selects a valid PCI config register for this chipset.
        ((unsafe { fstart_pio::pci_cfg_read32(bus, dev, func, aligned) } >> shift) & 0xff) as u8
    }

    fn pci_write8(bus: u8, dev: u8, func: u8, reg: u8, val: u8) {
        let aligned = reg & !3;
        let shift = ((reg & 3) as u32) * 8;
        // SAFETY: caller selects a valid PCI config register for this chipset.
        let old = unsafe { fstart_pio::pci_cfg_read32(bus, dev, func, aligned) };
        let mask = !(0xffu32 << shift);
        let new = (old & mask) | ((val as u32) << shift);
        unsafe { fstart_pio::pci_cfg_write32(bus, dev, func, aligned, new) };
    }

    fn pci_read_host8(reg: u8) -> u8 {
        Self::pci_read8(0, 0, 0, reg)
    }

    fn pci_write_host8(reg: u8, val: u8) {
        Self::pci_write8(0, 0, 0, reg, val);
    }

    fn pci_write_lpc32(reg: u8, val: u32) {
        // SAFETY: bus 0/device 31/function 0 is the ICH9 LPC bridge on Q35.
        unsafe { fstart_pio::pci_cfg_write32(0, ICH9_LPC_DEV, ICH9_LPC_FUNC, reg, val) }
    }

    fn pci_write_lpc8(reg: u8, val: u8) {
        Self::pci_write8(0, ICH9_LPC_DEV, ICH9_LPC_FUNC, reg, val);
    }

    fn decode_tseg_size(&self) -> usize {
        let mut esmramc = Self::pci_read_host8(ESMRAMC);
        // fstart's Q35 path always uses TSEG for permanent SMRAM.  If QEMU
        // has not yet reflected T_EN, decode the configured size anyway and
        // enable TSEG in `smm_close()` after installation.
        esmramc |= T_EN;
        match (esmramc & TSEG_SZ_MASK) >> 1 {
            0 => 1 << 20,
            1 => 2 << 20,
            2 => 8 << 20,
            _ => {
                let mb = self
                    .ecam
                    .config_read16(PciAddr::new(0, 0, 0), EXT_TSEG_MBYTES as u16)
                    .unwrap_or(8);
                (mb as usize) << 20
            }
        }
    }

    fn tseg_base_from_e820(&self, size: usize) -> u64 {
        let state = unsafe { fstart_services::memory_detect::e820_state() };
        let entries = state.entries();
        let size_u64 = size as u64;

        // Prefer an explicit reserved E820 region of the right size at the
        // top of low memory.  QEMU/coreboot often report TSEG this way.
        for entry in entries {
            if entry.kind != 1 && entry.size == size_u64 && entry.addr < 0x1_0000_0000 {
                return entry.addr;
            }
        }

        // Fallback: compute from the top of usable RAM below 4 GiB.
        let (tolud, _) = Self::ram_tops_from_e820(entries);
        tolud.saturating_sub(size_u64)
    }

    fn setup_ich9_pm_io(&self) {
        Self::pci_write_lpc32(ICH9_PMBASE_REG, Q35_PMBASE as u32 | 1);
        Self::pci_write_lpc8(ICH9_ACPI_CNTL, 0x80);
    }

    fn smm_open(&self) {
        // Open ASEG-compatible SMRAM access and disable TSEG hiding while the
        // BSP copies the permanent image.  Mirrors coreboot q35 `smm_open()`.
        Self::pci_write_host8(SMRAMC, D_OPEN | G_SMRAME | C_BASE_SEG);
        let esmramc = Self::pci_read_host8(ESMRAMC);
        Self::pci_write_host8(ESMRAMC, esmramc & !T_EN);
    }

    fn smm_close(&self) {
        Self::pci_write_host8(SMRAMC, G_SMRAME | C_BASE_SEG);
        let esmramc = Self::pci_read_host8(ESMRAMC);
        Self::pci_write_host8(ESMRAMC, esmramc | T_EN);
    }

    fn smm_lock(&self) {
        Self::pci_write_host8(SMRAMC, D_LCK | G_SMRAME | C_BASE_SEG);
    }

    fn cr3() -> u64 {
        let cr3: u64;
        // SAFETY: reading CR3 is safe in firmware privileged mode.
        unsafe {
            core::arch::asm!("mov {}, cr3", out(reg) cr3, options(nomem, nostack, preserves_flags));
        }
        cr3
    }
}

impl SmmOps for Q35HostBridge {
    fn smm_info(&self) -> Option<SmmInfo> {
        let size = self.decode_tseg_size();
        if size == 0 {
            return None;
        }
        let base = self.tseg_base_from_e820(size);
        fstart_log::info!("Q35 SMM: TSEG base={:#x} size={:#x}", base, size);
        Some(SmmInfo {
            smbase: base,
            smsize: size,
            save_state_size: AMD64_SAVE_STATE_SIZE,
        })
    }

    fn install_smm_handlers(
        &self,
        info: &SmmInfo,
        num_cpus: u16,
        image: &[u8],
    ) -> Result<(), SmmError> {
        self.smm_open();

        let layouts = unsafe { &mut *Q35_SMM_CPU_LAYOUTS.0.get() };
        let result = unsafe {
            fstart_smm::install_pic_image(
                image,
                fstart_smm::InstallConfig {
                    smram_base: info.smbase,
                    smram_size: info.smsize as u64,
                    num_cpus,
                    save_state_size: info.save_state_size as u32,
                    page_table_size: 0,
                    cr3: Self::cr3(),
                    platform_kind: fstart_smm::SMM_PLATFORM_INTEL_ICH,
                    platform_flags: 0,
                    platform_data: [Q35_PMBASE as u64, 0x20, 0, 0],
                },
                layouts,
            )
        };

        match result {
            Ok(installed) => {
                let targets = &installed.cpus[..num_cpus as usize];
                let smbases = unsafe { &mut *Q35_SMM_RELOCATION_SMBASES.0.get() };
                smbases.fill(targets[0].smbase);
                for (dst, cpu) in smbases.iter_mut().zip(targets.iter()) {
                    *dst = cpu.smbase;
                }
                let default_handler = unsafe {
                    fstart_smm::install_default_relocation_table_handler(
                        fstart_smm::DefaultRelocationTableConfig {
                            default_smbase: SMM_DEFAULT_SMBASE,
                            target_smbases: smbases,
                            save_state_smbase_offset: AMD64_SMBASE_SAVE_STATE_OFFSET,
                        },
                    )
                };
                if default_handler.is_err() {
                    self.smm_close();
                    fstart_log::error!("Q35 SMM: failed to install default relocation handler");
                    return Err(SmmError::InstallFailed);
                }

                fstart_log::info!(
                    "Q35 SMM: installed image common={:#x} entry={:#x} cpus={}",
                    installed.common_base,
                    installed.common_entry,
                    installed.cpus.len()
                );
                Ok(())
            }
            Err(_) => {
                self.smm_close();
                fstart_log::error!("Q35 SMM: failed to install SMM image");
                Err(SmmError::InstallFailed)
            }
        }
    }

    fn smm_relocate(&self) {
        // APMC writes synchronously trigger software SMI when APMC_EN is set.
        // Refresh EOS before every relocation SMI so multi-CPU relocation is
        // not blocked by the previous SMI cycle.
        let pm = fstart_pmio_ich::PmIo::new(Q35_PMBASE);
        pm.setbits32(
            fstart_pmio_ich::SMI_EN,
            fstart_pmio_ich::APMC_EN | fstart_pmio_ich::GBL_SMI_EN | fstart_pmio_ich::EOS,
        );
        unsafe { fstart_pio::outb(APM_CNT, APM_CNT_SMI) };
        // QEMU TCG may defer SMI injection until the translation block ends.
        // `pause` forces a new TB so each CPU takes the SMI before it advances
        // to the next MP rendezvous step.
        core::hint::spin_loop();
    }

    fn pre_smm_init(&self) {
        self.setup_ich9_pm_io();
        let pm = fstart_pmio_ich::PmIo::new(Q35_PMBASE);
        pm.reset_smi_status();
        pm.write32(
            fstart_pmio_ich::SMI_EN,
            fstart_pmio_ich::APMC_EN | fstart_pmio_ich::GBL_SMI_EN | fstart_pmio_ich::EOS,
        );
    }

    fn post_smm_init(&self) {
        self.smm_close();
        let pm = fstart_pmio_ich::PmIo::new(Q35_PMBASE);
        pm.write32(
            fstart_pmio_ich::SMI_EN,
            fstart_pmio_ich::APMC_EN | fstart_pmio_ich::GBL_SMI_EN | fstart_pmio_ich::EOS,
        );
        self.smm_lock();
        fstart_log::info!("Q35 SMM: global SMI enabled and SMRAM locked");
    }
}

// -----------------------------------------------------------------------
// Device trait
// -----------------------------------------------------------------------

impl Device for Q35HostBridge {
    const NAME: &'static str = "q35-hostbridge";
    const COMPATIBLE: &'static [&'static str] = &["q35-hostbridge"];
    type Config = Q35HostBridgeConfig;

    fn new(config: &Q35HostBridgeConfig) -> Result<Self, DeviceError> {
        // Create the inner PciEcam with zero-sized windows.  The real
        // windows are set by init_with_e820() before enumeration.
        let ecam_config = PciEcamConfig {
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
        };
        let ecam = PciEcam::new(&ecam_config)?;

        Ok(Self {
            config: *config,
            ecam,
        })
    }

    fn init(&mut self) -> Result<(), DeviceError> {
        // Read e820 data from the global state populated by MemoryDetect.
        // SAFETY: single-threaded firmware init; MemoryDetect runs before
        // PciInit in the capability pipeline order.
        let state = unsafe { fstart_services::memory_detect::e820_state() };
        if state.count() == 0 {
            fstart_log::error!(
                "Q35HostBridge::init(): no e820 data available. \
                 Ensure MemoryDetect runs before PciInit."
            );
            return Err(DeviceError::InitFailed);
        }
        self.init_with_e820(state.entries())
    }
}

// -----------------------------------------------------------------------
// PciRootBus — delegate to inner PciEcam
// -----------------------------------------------------------------------

impl PciRootBus for Q35HostBridge {
    fn config_read32(&self, addr: PciAddr, reg: u16) -> Result<u32, ServiceError> {
        self.ecam.config_read32(addr, reg)
    }

    fn config_write32(&self, addr: PciAddr, reg: u16, val: u32) -> Result<(), ServiceError> {
        self.ecam.config_write32(addr, reg, val)
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
