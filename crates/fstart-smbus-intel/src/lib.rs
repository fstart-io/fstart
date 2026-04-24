//! Intel I801 SMBus host controller driver.
//!
//! Implements byte- and word-level SMBus transactions over the I801
//! controller found in ICH7, ICH9, NM10, and PCH southbridges.
//!
//! The controller is accessed via legacy x86 I/O ports at a base address
//! programmed into PCI function 1F:3. The standard ICH7 base is `0x0400`.
//!
//! Ported from coreboot `src/southbridge/intel/common/smbus.c`.

#![no_std]

use fstart_pineview_regs::ecam;
use fstart_services::{ServiceError, SmBus};

// ---------------------------------------------------------------------------
// SMBus host register offsets (from the I/O base)
// ---------------------------------------------------------------------------

const SMBHSTSTAT: u16 = 0x00;
const SMBHSTCTL: u16 = 0x02;
const SMBHSTCMD: u16 = 0x03;
const SMBXMITADD: u16 = 0x04;
const SMBHSTDAT0: u16 = 0x05;
const SMBHSTDAT1: u16 = 0x06;
#[allow(dead_code)]
const SMBBLKDAT: u16 = 0x07;

// ---------------------------------------------------------------------------
// I801 command types (written to SMBHSTCTL bits [4:2])
// ---------------------------------------------------------------------------

#[allow(dead_code)]
const I801_QUICK: u8 = 0 << 2;
#[allow(dead_code)]
const I801_BYTE: u8 = 1 << 2;
const I801_BYTE_DATA: u8 = 2 << 2;
const I801_WORD_DATA: u8 = 3 << 2;

// ---------------------------------------------------------------------------
// Host status register bits
// ---------------------------------------------------------------------------

const SMBHSTSTS_HOST_BUSY: u8 = 1 << 0;
const SMBHSTSTS_INTR: u8 = 1 << 1;
const SMBHSTSTS_DEV_ERR: u8 = 1 << 2;
const SMBHSTSTS_BUS_ERR: u8 = 1 << 3;
const SMBHSTSTS_FAILED: u8 = 1 << 4;

// ---------------------------------------------------------------------------
// Host control register bits
// ---------------------------------------------------------------------------

const SMBHSTCNT_START: u8 = 1 << 6;

// ---------------------------------------------------------------------------
// Timeout (spin-loop iterations)
// ---------------------------------------------------------------------------

const SMBUS_TIMEOUT: u32 = 10_000_000;

// ---------------------------------------------------------------------------
// Address encoding
// ---------------------------------------------------------------------------

#[inline]
const fn xmit_read(addr: u8) -> u8 {
    (addr << 1) | 1
}
#[inline]
const fn xmit_write(addr: u8) -> u8 {
    addr << 1
}

// ---------------------------------------------------------------------------
// Driver
// ---------------------------------------------------------------------------

/// Intel I801 SMBus host controller.
///
/// Holds the I/O port base address and provides byte/word read/write
/// transactions. Constructed via [`I801SmBus::new`] (known base) or
/// [`I801SmBus::enable_on_ich7`] (auto-configure via PCI config space).
pub struct I801SmBus {
    base: u16,
}

// SAFETY: All state is CPU-exclusive during firmware; I/O port access
// is inherently single-threaded in the firmware context.
unsafe impl Send for I801SmBus {}
unsafe impl Sync for I801SmBus {}

impl I801SmBus {
    /// Create a driver with a known I/O base.
    pub const fn new(base: u16) -> Self {
        Self { base }
    }

    /// Enable the SMBus controller on an ICH7 southbridge via ECAM
    /// and return a ready-to-use driver.
    ///
    /// Programs PCI function 00:1F.3 through ECAM MMIO:
    /// - SMB_BASE (reg 0x20) = `smbus_base` | 1
    /// - HOSTC (reg 0x40) = HST_EN (1)
    /// - PCI_COMMAND |= I/O space enable
    /// Then resets the host controller.
    ///
    /// The ECAM must already be enabled (PCIEXBAR programmed) before
    /// calling this.
    pub fn enable_on_ich7(smbus_base: u16) -> Self {
        use fstart_pineview_regs::ich7;
        let smbus_pci = ecam::PciDevBdf::new(0, ich7::SMBUS_DEV, ich7::SMBUS_FUNC);
        smbus_pci.write32(ich7::SMB_BASE, (smbus_base as u32) | 1);
        smbus_pci.write32(ich7::HOSTC, ich7::HST_EN as u32);
        let cmd = smbus_pci.read16(ich7::PCI_COMMAND);
        smbus_pci.write16(ich7::PCI_COMMAND, cmd | ich7::PCI_CMD_IO);
        let s = Self { base: smbus_base };
        s.host_reset();
        fstart_log::info!("i801-smbus: enabled at I/O base {:#x}", smbus_base);
        s
    }

    /// Reset the SMBus host controller.
    ///
    /// Disables interrupts and clears any lingering status bits so
    /// new transactions can run.
    pub fn host_reset(&self) {
        #[cfg(target_arch = "x86_64")]
        {
            // SAFETY: base points to the SMBus I/O region programmed
            // by enable_on_ich7 / the board config.
            unsafe {
                fstart_pio::outb(self.base + SMBHSTCTL, 0);
                // Write-to-clear all status bits.
                let stat = fstart_pio::inb(self.base + SMBHSTSTAT);
                fstart_pio::outb(self.base + SMBHSTSTAT, stat);
            }
        }
    }

    /// Spin until the host controller is not busy.
    fn wait_not_busy(&self) -> Result<(), ServiceError> {
        #[cfg(target_arch = "x86_64")]
        {
            let mut loops = SMBUS_TIMEOUT;
            loop {
                // SAFETY: base is the SMBus I/O region.
                let stat = unsafe { fstart_pio::inb(self.base + SMBHSTSTAT) };
                if stat & SMBHSTSTS_HOST_BUSY == 0 {
                    return Ok(());
                }
                loops -= 1;
                if loops == 0 {
                    fstart_log::error!("i801-smbus: timeout waiting for not-busy");
                    return Err(ServiceError::Timeout);
                }
                core::hint::spin_loop();
            }
        }
        #[cfg(not(target_arch = "x86_64"))]
        Err(ServiceError::HardwareError)
    }

    /// Set up the command, wait for not-busy, clear status, write
    /// the control and address registers.
    fn setup_command(&self, ctrl: u8, xmitadd: u8) -> Result<(), ServiceError> {
        self.wait_not_busy()?;
        #[cfg(target_arch = "x86_64")]
        // SAFETY: base is the SMBus I/O region.
        unsafe {
            // Clear any lingering status.
            let stat = fstart_pio::inb(self.base + SMBHSTSTAT);
            fstart_pio::outb(self.base + SMBHSTSTAT, stat);
            // Set transaction type (disable interrupts).
            fstart_pio::outb(self.base + SMBHSTCTL, ctrl);
            // Set slave address + R/W bit.
            fstart_pio::outb(self.base + SMBXMITADD, xmitadd);
        }
        Ok(())
    }

    /// Start the transaction and wait for completion.
    fn execute_and_complete(&self) -> Result<(), ServiceError> {
        #[cfg(target_arch = "x86_64")]
        {
            // SAFETY: base is the SMBus I/O region.
            unsafe {
                // Start.
                let ctl = fstart_pio::inb(self.base + SMBHSTCTL);
                fstart_pio::outb(self.base + SMBHSTCTL, ctl | SMBHSTCNT_START);
            }
            // Wait for the controller to signal activity.
            let mut loops = SMBUS_TIMEOUT;
            loop {
                // SAFETY: base is the SMBus I/O region.
                let stat = unsafe { fstart_pio::inb(self.base + SMBHSTSTAT) };
                // Mask out non-completion bits.
                let relevant = stat & !(SMBHSTSTS_HOST_BUSY);
                if relevant != 0 {
                    // Check for errors.
                    if stat & (SMBHSTSTS_DEV_ERR | SMBHSTSTS_BUS_ERR | SMBHSTSTS_FAILED) != 0 {
                        fstart_log::error!("i801-smbus: transaction error, status={:#x}", stat);
                        return Err(ServiceError::HardwareError);
                    }
                    // Wait for host to finish (not busy).
                    if stat & SMBHSTSTS_HOST_BUSY == 0 {
                        return Ok(());
                    }
                }
                loops -= 1;
                if loops == 0 {
                    fstart_log::error!("i801-smbus: timeout waiting for completion");
                    return Err(ServiceError::Timeout);
                }
                core::hint::spin_loop();
            }
        }
        #[cfg(not(target_arch = "x86_64"))]
        Err(ServiceError::HardwareError)
    }

    /// Read a byte via I801_BYTE_DATA command.
    pub fn read_byte_data(&self, addr: u8, cmd: u8) -> Result<u8, ServiceError> {
        self.setup_command(I801_BYTE_DATA, xmit_read(addr))?;
        #[cfg(target_arch = "x86_64")]
        // SAFETY: base is the SMBus I/O region.
        unsafe {
            fstart_pio::outb(self.base + SMBHSTCMD, cmd);
            fstart_pio::outb(self.base + SMBHSTDAT0, 0);
            fstart_pio::outb(self.base + SMBHSTDAT1, 0);
        }
        self.execute_and_complete()?;
        #[cfg(target_arch = "x86_64")]
        {
            // SAFETY: base is the SMBus I/O region.
            let val = unsafe { fstart_pio::inb(self.base + SMBHSTDAT0) };
            Ok(val)
        }
        #[cfg(not(target_arch = "x86_64"))]
        Err(ServiceError::HardwareError)
    }

    /// Write a byte via I801_BYTE_DATA command.
    pub fn write_byte_data(&self, addr: u8, cmd: u8, val: u8) -> Result<(), ServiceError> {
        self.setup_command(I801_BYTE_DATA, xmit_write(addr))?;
        #[cfg(target_arch = "x86_64")]
        // SAFETY: base is the SMBus I/O region.
        unsafe {
            fstart_pio::outb(self.base + SMBHSTCMD, cmd);
            fstart_pio::outb(self.base + SMBHSTDAT0, val);
        }
        self.execute_and_complete()
    }

    /// Read a 16-bit word via I801_WORD_DATA command.
    pub fn read_word_data(&self, addr: u8, cmd: u8) -> Result<u16, ServiceError> {
        self.setup_command(I801_WORD_DATA, xmit_read(addr))?;
        #[cfg(target_arch = "x86_64")]
        // SAFETY: base is the SMBus I/O region.
        unsafe {
            fstart_pio::outb(self.base + SMBHSTCMD, cmd);
            fstart_pio::outb(self.base + SMBHSTDAT0, 0);
            fstart_pio::outb(self.base + SMBHSTDAT1, 0);
        }
        self.execute_and_complete()?;
        #[cfg(target_arch = "x86_64")]
        {
            // SAFETY: base is the SMBus I/O region.
            let lo = unsafe { fstart_pio::inb(self.base + SMBHSTDAT0) };
            let hi = unsafe { fstart_pio::inb(self.base + SMBHSTDAT1) };
            Ok((hi as u16) << 8 | lo as u16)
        }
        #[cfg(not(target_arch = "x86_64"))]
        Err(ServiceError::HardwareError)
    }
}

impl SmBus for I801SmBus {
    fn read_byte(&mut self, addr: u8, cmd: u8) -> Result<u8, ServiceError> {
        self.read_byte_data(addr, cmd)
    }
    fn write_byte(&mut self, addr: u8, cmd: u8, value: u8) -> Result<(), ServiceError> {
        self.write_byte_data(addr, cmd, value)
    }
    fn read_word(&mut self, addr: u8, cmd: u8) -> Result<u16, ServiceError> {
        self.read_word_data(addr, cmd)
    }
}
