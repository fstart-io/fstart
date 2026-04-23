//! ACPI PM I/O register abstraction for Intel ICH southbridges.
//!
//! Provides a [`PmIo`] accessor for the PMBASE I/O region (default 0x500,
//! 128 bytes) and a [`TcoIo`] sub-accessor for the TCO register block at
//! PMBASE + 0x60.
//!
//! Covers ICH7 through ICH10/NM10 and Intel 5/6-series PCH.  Register
//! offsets and bit definitions follow coreboot's `pmutil.h`.
//!
//! # Usage
//!
//! ```ignore
//! let pm = PmIo::new(0x0500);
//! let sts = pm.read16(pm::PM1_STS);
//! pm.write16(pm::PM1_STS, sts); // W1C
//!
//! let tco = pm.tco();
//! let tco1 = tco.read16(tco::TCO1_STS);
//! ```

#![no_std]

// -----------------------------------------------------------------------
// PM register offsets from PMBASE
// -----------------------------------------------------------------------

/// PM1 Status register (16-bit, W1C).
pub const PM1_STS: u16 = 0x00;
/// PM1 Enable register (16-bit).
pub const PM1_EN: u16 = 0x02;
/// PM1 Control register (32-bit).
pub const PM1_CNT: u16 = 0x04;
/// PM Timer (32-bit, read-only).
pub const PM1_TMR: u16 = 0x08;
/// Processor Control register.
pub const PROC_CNT: u16 = 0x10;

/// GPE0 Status register (32-bit, W1C).
///
/// ICH7 uses a 32-bit GPE0 at offset 0x28.
/// ICH8+ uses 64-bit GPE0 at offset 0x20 (low) and 0x24 (high).
pub const GPE0_STS: u16 = 0x28;
/// GPE0 Enable register (32-bit).
pub const GPE0_EN: u16 = 0x2C;

/// SMI Enable register (32-bit).
pub const SMI_EN: u16 = 0x30;
/// SMI Status register (32-bit, W1C).
pub const SMI_STS: u16 = 0x34;
/// Alternate GPI SMI Enable register (16-bit).
pub const ALT_GP_SMI_EN: u16 = 0x38;
/// Alternate GPI SMI Status register (16-bit, W1C).
pub const ALT_GP_SMI_STS: u16 = 0x3A;
/// GPE Control register.
pub const GPE_CNTL: u16 = 0x42;
/// Device Activity Status.
pub const DEVACT_STS: u16 = 0x44;
/// PM2 Control (mobile only, ICH7 at 0x20).
pub const PM2_CNT: u16 = 0x50;

// -----------------------------------------------------------------------
// PM1_STS bits
// -----------------------------------------------------------------------
pub const WAK_STS: u16 = 1 << 15;
pub const PCIEXPWAK_STS: u16 = 1 << 14;
pub const PRBTNOR_STS: u16 = 1 << 11;
pub const RTC_STS: u16 = 1 << 10;
pub const PWRBTN_STS: u16 = 1 << 8;
pub const GBL_STS: u16 = 1 << 5;
pub const BM_STS: u16 = 1 << 4;
pub const TMROF_STS: u16 = 1 << 0;

// -----------------------------------------------------------------------
// PM1_EN bits
// -----------------------------------------------------------------------
pub const PCIEXPWAK_DIS: u16 = 1 << 14;
pub const RTC_EN: u16 = 1 << 10;
pub const PWRBTN_EN: u16 = 1 << 8;
pub const GBL_EN: u16 = 1 << 5;
pub const TMROF_EN: u16 = 1 << 0;

// -----------------------------------------------------------------------
// PM1_CNT bits
// -----------------------------------------------------------------------
/// Sleep Type field mask (bits 12:10).
pub const SLP_TYP_MASK: u32 = 0x1C00;
/// Sleep Type shift.
pub const SLP_TYP_SHIFT: u32 = 10;
/// Sleep Enable bit (bit 13). Writing 1 enters the sleep state in SLP_TYP.
pub const SLP_EN: u32 = 1 << 13;
/// Global Release (bit 2).
pub const GBL_RLS: u32 = 1 << 2;
/// Bus Master Reload (bit 1).
pub const BM_RLD: u32 = 1 << 1;
/// SCI Enable (bit 0).
pub const SCI_EN: u32 = 1 << 0;

// -----------------------------------------------------------------------
// SMI_EN bits
// -----------------------------------------------------------------------
pub const GBL_SMI_EN: u32 = 1 << 0;
pub const EOS: u32 = 1 << 1;
pub const BIOS_EN: u32 = 1 << 2;
pub const LEGACY_USB_EN: u32 = 1 << 3;
pub const SLP_SMI_EN: u32 = 1 << 4;
pub const APMC_EN: u32 = 1 << 5;
pub const SWSMI_TMR_EN: u32 = 1 << 6;
pub const BIOS_RLS: u32 = 1 << 7;
pub const MCSMI_EN: u32 = 1 << 11;
pub const TCO_EN: u32 = 1 << 13;
pub const PERIODIC_EN: u32 = 1 << 14;
pub const LEGACY_USB2_EN: u32 = 1 << 17;
pub const INTEL_USB2_EN: u32 = 1 << 18;

// -----------------------------------------------------------------------
// GPE0_STS bits (ICH7 32-bit layout)
// -----------------------------------------------------------------------
pub const THRM_STS: u32 = 1 << 0;
pub const HOT_PLUG_STS: u32 = 1 << 1;
pub const SWGPE_STS: u32 = 1 << 2;
pub const TCOSCI_STS: u32 = 1 << 6;
pub const SMB_WAK_STS: u32 = 1 << 7;
pub const RI_STS: u32 = 1 << 8;
pub const PCI_EXP_STS: u32 = 1 << 9;
pub const BATLOW_STS: u32 = 1 << 10;
pub const PME_STS: u32 = 1 << 11;
pub const PME_B0_STS: u32 = 1 << 13;
pub const USB4_STS: u32 = 1 << 14;

// -----------------------------------------------------------------------
// TCO register offsets (from PMBASE + 0x60)
// -----------------------------------------------------------------------

/// TCO I/O block offset from PMBASE.
pub const TCO_BASE_OFFSET: u16 = 0x60;

/// TCO1 Status (16-bit, W1C).
pub const TCO1_STS: u16 = 0x04;
/// TCO2 Status (16-bit, W1C).
pub const TCO2_STS: u16 = 0x06;
/// TCO1 Control (16-bit).
pub const TCO1_CNT: u16 = 0x08;
/// TCO2 Control (16-bit).
pub const TCO2_CNT: u16 = 0x0A;

// TCO1_STS bits
pub const TIMEOUT_STS: u32 = 1 << 3;
pub const TCO_INT_STS: u32 = 1 << 2;
pub const SW_TCO_STS: u32 = 1 << 1;
pub const NMI2SMI_STS: u32 = 1 << 0;

// TCO1_CNT bits
pub const TCO_LOCK: u16 = 1 << 12;

// TCO combined (TCO1_STS + TCO2_STS as u32)
pub const BOOT_STS: u32 = 1 << 18;
pub const SECOND_TO_STS: u32 = 1 << 17;
pub const DMISCI_STS: u32 = 1 << 9;

/// Total PMBASE I/O region size (128 bytes).
const PMSIZE: u16 = 0x80;

// =======================================================================
// PmIo accessor
// =======================================================================

/// ACPI PM I/O register accessor.
///
/// Wraps a PMBASE I/O port address and provides typed read/write methods
/// for PM1, GPE0, SMI, and ALT_GP registers.  All offsets are
/// bounds-checked against the 128-byte PMBASE region.
///
/// # Example
///
/// ```ignore
/// use fstart_pmio_ich::{PmIo, PM1_STS, WAK_STS};
///
/// let pm = PmIo::new(0x0500);
///
/// // Clear wake status
/// let sts = pm.read16(PM1_STS);
/// if sts & WAK_STS != 0 {
///     pm.write16(PM1_STS, WAK_STS);
/// }
/// ```
#[derive(Debug, Clone, Copy)]
pub struct PmIo {
    base: u16,
}

impl PmIo {
    /// Create a new PM I/O accessor.
    ///
    /// `base` is the PMBASE I/O port address (e.g., 0x0500 for ICH7).
    #[inline]
    pub const fn new(base: u16) -> Self {
        Self { base }
    }

    /// Return the PMBASE I/O port address.
    #[inline]
    pub const fn base(&self) -> u16 {
        self.base
    }

    /// Read a 32-bit PM register.
    #[inline]
    pub fn read32(&self, offset: u16) -> u32 {
        debug_assert!(offset + 4 <= PMSIZE);
        // SAFETY: caller constructed PmIo with a valid PMBASE.
        unsafe { fstart_pio::inl(self.base + offset) }
    }

    /// Write a 32-bit PM register.
    #[inline]
    pub fn write32(&self, offset: u16, val: u32) {
        debug_assert!(offset + 4 <= PMSIZE);
        // SAFETY: caller constructed PmIo with a valid PMBASE.
        unsafe { fstart_pio::outl(self.base + offset, val) }
    }

    /// Read a 16-bit PM register.
    #[inline]
    pub fn read16(&self, offset: u16) -> u16 {
        debug_assert!(offset + 2 <= PMSIZE);
        unsafe { fstart_pio::inw(self.base + offset) }
    }

    /// Write a 16-bit PM register.
    #[inline]
    pub fn write16(&self, offset: u16, val: u16) {
        debug_assert!(offset + 2 <= PMSIZE);
        unsafe { fstart_pio::outw(self.base + offset, val) }
    }

    /// Read an 8-bit PM register.
    #[inline]
    pub fn read8(&self, offset: u16) -> u8 {
        debug_assert!(offset < PMSIZE);
        unsafe { fstart_pio::inb(self.base + offset) }
    }

    /// Write an 8-bit PM register.
    #[inline]
    pub fn write8(&self, offset: u16, val: u8) {
        debug_assert!(offset < PMSIZE);
        unsafe { fstart_pio::outb(self.base + offset, val) }
    }

    /// Set bits in a 32-bit PM register.
    #[inline]
    pub fn setbits32(&self, offset: u16, bits: u32) {
        let v = self.read32(offset);
        self.write32(offset, v | bits);
    }

    /// Clear bits in a 32-bit PM register.
    #[inline]
    pub fn clrbits32(&self, offset: u16, bits: u32) {
        let v = self.read32(offset);
        self.write32(offset, v & !bits);
    }

    /// Set bits in a 16-bit PM register.
    #[inline]
    pub fn setbits16(&self, offset: u16, bits: u16) {
        let v = self.read16(offset);
        self.write16(offset, v | bits);
    }

    /// Clear bits in a 16-bit PM register.
    #[inline]
    pub fn clrbits16(&self, offset: u16, bits: u16) {
        let v = self.read16(offset);
        self.write16(offset, v & !bits);
    }

    // -------------------------------------------------------------------
    // Named register helpers (coreboot pmutil.c equivalents)
    // -------------------------------------------------------------------

    /// Read and clear PM1_STS (write-1-to-clear).
    pub fn reset_pm1_status(&self) -> u16 {
        let sts = self.read16(PM1_STS);
        self.write16(PM1_STS, sts);
        sts
    }

    /// Read and clear SMI_STS (write-1-to-clear).
    pub fn reset_smi_status(&self) -> u32 {
        let sts = self.read32(SMI_STS);
        self.write32(SMI_STS, sts);
        sts
    }

    /// Read and clear GPE0_STS (write-1-to-clear).
    pub fn reset_gpe0_status(&self) -> u32 {
        let sts = self.read32(GPE0_STS);
        self.write32(GPE0_STS, sts);
        sts
    }

    /// Read and clear ALT_GP_SMI_STS (write-1-to-clear).
    pub fn reset_alt_gp_smi_status(&self) -> u16 {
        let sts = self.read16(ALT_GP_SMI_STS);
        self.write16(ALT_GP_SMI_STS, sts);
        sts
    }

    /// Enable global SMI generation.
    pub fn global_smi_enable(&self) {
        self.setbits32(SMI_EN, GBL_SMI_EN | EOS);
    }

    /// Mask GPE0 events: clear `clr` bits, set `set` bits.
    pub fn gpe0_mask(&self, clr: u32, set: u32) {
        let v = self.read32(GPE0_EN);
        self.write32(GPE0_EN, (v & !clr) | set);
    }

    /// Mask ALT_GP_SMI events: clear `clr` bits, set `set` bits.
    pub fn alt_gpi_mask(&self, clr: u16, set: u16) {
        let v = self.read16(ALT_GP_SMI_EN);
        self.write16(ALT_GP_SMI_EN, (v & !clr) | set);
    }

    /// Extract SLP_TYP from PM1_CNT.
    pub fn sleep_type(&self) -> u32 {
        (self.read32(PM1_CNT) & SLP_TYP_MASK) >> SLP_TYP_SHIFT
    }

    /// Check if the system is waking from S3 (PM1_STS.WAK_STS set and
    /// PM1_CNT.SLP_TYP == 5).
    pub fn is_s3_resume(&self) -> bool {
        let sts = self.read16(PM1_STS);
        if sts & WAK_STS == 0 {
            return false;
        }
        self.sleep_type() == 5
    }

    /// Enter S5 (soft-off).
    pub fn poweroff(&self) -> ! {
        let mut pm1 = self.read32(PM1_CNT);
        pm1 &= !SLP_TYP_MASK;
        pm1 |= 7 << SLP_TYP_SHIFT; // S5
        pm1 |= SLP_EN;
        self.write32(PM1_CNT, pm1);
        loop {
            core::hint::spin_loop();
        }
    }

    /// Get a [`TcoIo`] sub-accessor for the TCO register block.
    #[inline]
    pub const fn tco(&self) -> TcoIo {
        TcoIo {
            base: self.base + TCO_BASE_OFFSET,
        }
    }
}

// =======================================================================
// TcoIo sub-accessor
// =======================================================================

/// TCO I/O register accessor (PMBASE + 0x60, 32 bytes).
///
/// TCO (Total Cost of Ownership) registers handle the watchdog timer,
/// boot status, BIOS write protection, and NMI routing.
#[derive(Debug, Clone, Copy)]
pub struct TcoIo {
    base: u16,
}

impl TcoIo {
    /// Read a 16-bit TCO register.
    #[inline]
    pub fn read16(&self, offset: u16) -> u16 {
        debug_assert!(offset + 2 <= 0x20);
        unsafe { fstart_pio::inw(self.base + offset) }
    }

    /// Write a 16-bit TCO register.
    #[inline]
    pub fn write16(&self, offset: u16, val: u16) {
        debug_assert!(offset + 2 <= 0x20);
        unsafe { fstart_pio::outw(self.base + offset, val) }
    }

    /// Read a 32-bit TCO register (TCO1_STS + TCO2_STS combined).
    #[inline]
    pub fn read32(&self, offset: u16) -> u32 {
        debug_assert!(offset + 4 <= 0x20);
        unsafe { fstart_pio::inl(self.base + offset) }
    }

    /// Write a 32-bit TCO register.
    #[inline]
    pub fn write32(&self, offset: u16, val: u32) {
        debug_assert!(offset + 4 <= 0x20);
        unsafe { fstart_pio::outl(self.base + offset, val) }
    }

    /// Read and clear TCO status.
    ///
    /// Handles the BOOT_STS ordering requirement: clear other bits first,
    /// then clear BOOT_STS (must be cleared after SECOND_TO_STS).
    pub fn reset_tco_status(&self) -> u32 {
        let sts = self.read32(TCO1_STS);
        // Clear everything except BOOT_STS first.
        self.write32(TCO1_STS, sts & !BOOT_STS);
        // Then clear BOOT_STS if set.
        if sts & BOOT_STS != 0 {
            self.write32(TCO1_STS, BOOT_STS);
        }
        sts
    }

    /// Lock TCO registers (set TCO_LOCK in TCO1_CNT).
    pub fn lock(&self) {
        let v = self.read16(TCO1_CNT);
        self.write16(TCO1_CNT, v | TCO_LOCK);
    }
}

// SAFETY: PmIo/TcoIo are just I/O port base wrappers with no interior state.
unsafe impl Send for PmIo {}
unsafe impl Sync for PmIo {}
unsafe impl Send for TcoIo {}
unsafe impl Sync for TcoIo {}
