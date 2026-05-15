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

use fstart_ecam as ecam;
use fstart_pio::{pio_register_structs, PioRegister};
use fstart_services::{ServiceError, SmBus};
use tock_registers::interfaces::{Readable, Writeable};
use tock_registers::register_bitfields;

register_bitfields![u8,
    HSTSTAT [
        HOST_BUSY OFFSET(0) NUMBITS(1) [],
        INTR OFFSET(1) NUMBITS(1) [],
        DEV_ERR OFFSET(2) NUMBITS(1) [],
        BUS_ERR OFFSET(3) NUMBITS(1) [],
        FAILED OFFSET(4) NUMBITS(1) [],
        SMBALERT_STS OFFSET(5) NUMBITS(1) [],
        INUSE_STS OFFSET(6) NUMBITS(1) [],
        BYTE_DONE OFFSET(7) NUMBITS(1) []
    ],
    HSTCTL [
        TYPE OFFSET(2) NUMBITS(3) [],
        START OFFSET(6) NUMBITS(1) []
    ]
];

pio_register_structs! {
    /// I801 SMBus host-controller I/O register block.
    I801Regs {
        (0x00 => status: PioRegister<u8, HSTSTAT::Register>),
        (0x02 => control: PioRegister<u8, HSTCTL::Register>),
        (0x03 => command: PioRegister<u8>),
        (0x04 => xmit_addr: PioRegister<u8>),
        (0x05 => data0: PioRegister<u8>),
        (0x06 => data1: PioRegister<u8>),
        (0x07 => block_data: PioRegister<u8>),
    }
}

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
const SMBHSTSTS_SMBALERT_STS: u8 = 1 << 5;
const SMBHSTSTS_INUSE_STS: u8 = 1 << 6;
const SMBHSTSTS_BYTE_DONE: u8 = 1 << 7;

const SMBHSTSTS_ERROR: u8 = SMBHSTSTS_DEV_ERR | SMBHSTSTS_BUS_ERR | SMBHSTSTS_FAILED;
const SMBHSTSTS_NON_COMPLETION: u8 =
    SMBHSTSTS_BYTE_DONE | SMBHSTSTS_INUSE_STS | SMBHSTSTS_SMBALERT_STS;

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
/// [`I801SmBus::enable_on_i801`] (auto-configure via PCI config space).
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

    #[inline(always)]
    fn regs(&self) -> I801Regs {
        I801Regs::new(self.base)
    }

    /// Enable an Intel I801-compatible SMBus controller via ECAM.
    ///
    /// This covers ICH7/NM10, ICH8/ICH8-M, ICH9, and later PCH parts that
    /// keep the SMBus function at a board/chipset-supplied BDF with the
    /// standard `SMB_BASE` (0x20), `HOSTC` (0x40) and PCI command registers.
    pub fn enable_on_i801(bus: u8, dev: u8, func: u8, smbus_base: u16) -> Self {
        const SMB_BASE: u16 = 0x20;
        const HOSTC: u16 = 0x40;
        const HST_EN: u32 = 1;
        const PCI_COMMAND: u16 = 0x04;
        const PCI_CMD_IO: u16 = 0x0001;

        let smbus_pci = ecam::PciDevBdf::new(bus, dev, func);
        smbus_pci.write32(SMB_BASE, (smbus_base as u32) | 1);
        smbus_pci.write32(HOSTC, HST_EN);
        let cmd = smbus_pci.read16(PCI_COMMAND);
        smbus_pci.write16(PCI_COMMAND, cmd | PCI_CMD_IO);
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
            let regs = self.regs();
            regs.control().set(0);
            let stat = regs.status().get();
            regs.status().set(stat);
        }
    }

    /// Spin until the host controller is not busy.
    fn wait_not_busy(&self) -> Result<(), ServiceError> {
        #[cfg(target_arch = "x86_64")]
        {
            let mut loops = SMBUS_TIMEOUT;
            loop {
                let stat = self.regs().status().get();
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
        {
            let regs = self.regs();
            let stat = regs.status().get();
            regs.status().set(stat);
            regs.control().set(ctrl);
            regs.xmit_addr().set(xmitadd);
        }
        Ok(())
    }

    /// Start the transaction and wait for completion.
    fn execute_and_complete(&self) -> Result<(), ServiceError> {
        #[cfg(target_arch = "x86_64")]
        {
            let regs = self.regs();
            let ctl = regs.control().get();
            regs.control().set(ctl | SMBHSTCNT_START);
            // Wait for the controller to signal activity.
            let mut loops = SMBUS_TIMEOUT;
            loop {
                let stat = regs.status().get();
                let completion = stat & !(SMBHSTSTS_HOST_BUSY | SMBHSTSTS_NON_COMPLETION);
                if completion != 0 && stat & SMBHSTSTS_HOST_BUSY == 0 {
                    if completion & SMBHSTSTS_ERROR == 0 && completion & SMBHSTSTS_INTR != 0 {
                        regs.status().set(stat);
                        return Ok(());
                    }
                    regs.status().set(stat);
                    fstart_log::error!("i801-smbus: transaction error, status={:#x}", stat);
                    return Err(ServiceError::HardwareError);
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
        {
            let regs = self.regs();
            regs.command().set(cmd);
            regs.data0().set(0);
            regs.data1().set(0);
        }
        self.execute_and_complete()?;
        #[cfg(target_arch = "x86_64")]
        {
            Ok(self.regs().data0().get())
        }
        #[cfg(not(target_arch = "x86_64"))]
        Err(ServiceError::HardwareError)
    }

    /// Write a byte via I801_BYTE_DATA command.
    pub fn write_byte_data(&self, addr: u8, cmd: u8, val: u8) -> Result<(), ServiceError> {
        self.setup_command(I801_BYTE_DATA, xmit_write(addr))?;
        #[cfg(target_arch = "x86_64")]
        {
            let regs = self.regs();
            regs.command().set(cmd);
            regs.data0().set(val);
        }
        self.execute_and_complete()
    }

    /// Read a 16-bit word via I801_WORD_DATA command.
    pub fn read_word_data(&self, addr: u8, cmd: u8) -> Result<u16, ServiceError> {
        self.setup_command(I801_WORD_DATA, xmit_read(addr))?;
        #[cfg(target_arch = "x86_64")]
        {
            let regs = self.regs();
            regs.command().set(cmd);
            regs.data0().set(0);
            regs.data1().set(0);
        }
        self.execute_and_complete()?;
        #[cfg(target_arch = "x86_64")]
        {
            let regs = self.regs();
            let lo = regs.data0().get();
            let hi = regs.data1().get();
            Ok((hi as u16) << 8 | lo as u16)
        }
        #[cfg(not(target_arch = "x86_64"))]
        Err(ServiceError::HardwareError)
    }

    /// Write a 16-bit word via I801_WORD_DATA command.
    pub fn write_word_data(&self, addr: u8, cmd: u8, val: u16) -> Result<(), ServiceError> {
        self.setup_command(I801_WORD_DATA, xmit_write(addr))?;
        #[cfg(target_arch = "x86_64")]
        {
            let regs = self.regs();
            regs.command().set(cmd);
            regs.data0().set(val as u8);
            regs.data1().set((val >> 8) as u8);
        }
        self.execute_and_complete()
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
    fn write_word(&mut self, addr: u8, cmd: u8, value: u16) -> Result<(), ServiceError> {
        self.write_word_data(addr, cmd, value)
    }
}
