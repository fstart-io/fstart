//! Intel I801 SMBus host controller driver.
//!
//! Implements byte- and word-level SMBus transactions over the I801
//! controller found in ICH7, ICH9, NM10, and PCH southbridges.
//!
//! The controller is accessed via legacy x86 I/O ports at a base address
//! programmed into PCI function 1F:3. The standard ICH7 base is `0x0400`.
//!
//! Ported from coreboot `src/southbridge/intel/common/smbus.c`.

use fstart_core::pio::PioRegister;
use fstart_core::pio_register_structs;
use fstart_core::services::{ServiceError, SmBus};
use fstart_pci::ecam;
use tock_registers::fields::FieldValue;
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};
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
        INTREN OFFSET(0) NUMBITS(1) [],
        KILL OFFSET(1) NUMBITS(1) [],
        TYPE OFFSET(2) NUMBITS(3) [
            Quick = 0, Byte = 1, ByteData = 2, WordData = 3,
            ProcessCall = 4, BlockData = 5, I2cBlockData = 6
        ],
        LAST_BYTE OFFSET(5) NUMBITS(1) [],
        START OFFSET(6) NUMBITS(1) [],
        PEC_EN OFFSET(7) NUMBITS(1) []
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

// Semantic status groups derive from the register fields, not another bit map.
const STATUS_ERROR: u8 =
    HSTSTAT::DEV_ERR::SET.value | HSTSTAT::BUS_ERR::SET.value | HSTSTAT::FAILED::SET.value;
const STATUS_NON_COMPLETION: u8 = HSTSTAT::BYTE_DONE::SET.value
    | HSTSTAT::INUSE_STS::SET.value
    | HSTSTAT::SMBALERT_STS::SET.value;

// ---------------------------------------------------------------------------
// Timeout (spin-loop iterations)
// ---------------------------------------------------------------------------

const SMBUS_TIMEOUT: u32 = 10_000_000;

/// Maximum data bytes in one SMBus block transfer (SMBus 2.0 limit). The
/// transfer additionally carries the device's count byte.
pub const SMBUS_BLOCK_MAXLEN: usize = 32;

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

/// How [`I801SmBus::block_cmd_loop`] drives the byte engine.
#[derive(Clone, Copy, PartialEq, Eq)]
enum BlockMode {
    /// SMBus block read: the device sends its byte count first.
    Read,
    /// SMBus block write: the host announces the byte count.
    Write,
    /// I2C read: no count byte; the host NAKs the last byte it wants.
    I2cRead,
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
    /// Config-space coordinates of the controller, when known. Used to
    /// diagnose and recover a controller left busy by a stuck transaction.
    bdf: Option<(u8, u8, u8)>,
}

// SAFETY: All state is CPU-exclusive during firmware; I/O port access
// is inherently single-threaded in the firmware context.
unsafe impl Send for I801SmBus {}
unsafe impl Sync for I801SmBus {}

impl I801SmBus {
    /// Create a driver with a known I/O base.
    pub const fn new(base: u16) -> Self {
        Self { base, bdf: None }
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
        const HST_EN: u8 = 1;
        const PCI_COMMAND: u16 = 0x04;
        const PCI_CMD_IO: u16 = 0x0001;

        let smbus_pci = ecam::EcamDevice::new(bus, dev, func);
        smbus_pci.write32(SMB_BASE, (smbus_base as u32) | 1);
        smbus_pci.write8(HOSTC, HST_EN);
        let cmd = smbus_pci.read16(PCI_COMMAND);
        smbus_pci.write16(PCI_COMMAND, cmd | PCI_CMD_IO);
        let s = Self {
            base: smbus_base,
            bdf: Some((bus, dev, func)),
        };
        s.host_reset();
        fstart_log::info!("i801-smbus: enabled at I/O base {:#x}", smbus_base);
        s
    }

    /// Reset the SMBus host controller to its normal idle programming.
    ///
    /// This mirrors coreboot's initialization: disable interrupts and clear
    /// the write-one-to-clear status bits. KILL is not part of initialization;
    /// it is only meaningful while aborting a known active transaction.
    pub fn host_reset(&self) {
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        {
            let regs = self.regs();
            regs.control().set(0);
            let stat = regs.status().get();
            regs.status().set(stat);
        }
    }

    /// Log the controller's config-space state, so a controller that refuses
    /// to become idle can be told apart from an undecoded I/O BAR.
    fn log_pci_state(&self, why: &str) {
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        if let Some((bus, dev, func)) = self.bdf {
            let pci = ecam::EcamDevice::new(bus, dev, func);
            fstart_log::warn!(
                "i801-smbus: {}: vid/did {:#06x}/{:#06x} cmd {:#06x} base {:#010x} hostc {:#010x}",
                why,
                pci.read16(0x00),
                pci.read16(0x02),
                pci.read16(0x04),
                pci.read32(0x20),
                pci.read32(0x40)
            );
        }
        #[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
        let _ = why;
    }

    /// Spin until the host controller is not busy.
    fn wait_not_busy(&self) -> Result<(), ServiceError> {
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        {
            let mut loops = SMBUS_TIMEOUT;
            loop {
                if !self.regs().status().is_set(HSTSTAT::HOST_BUSY) {
                    return Ok(());
                }
                loops -= 1;
                if loops == 0 {
                    // Clear any lingering completion/error status and check
                    // once more before declaring the controller dead.
                    fstart_log::warn!(
                        "i801-smbus: controller busy (sts {:#04x} ctl {:#04x})",
                        self.regs().status().get(),
                        self.regs().control().get()
                    );
                    self.log_pci_state("busy controller");
                    self.host_reset();
                    if !self.regs().status().is_set(HSTSTAT::HOST_BUSY) {
                        return Ok(());
                    }
                    fstart_log::error!("i801-smbus: timeout waiting for not-busy");
                    return Err(ServiceError::Timeout);
                }
                core::hint::spin_loop();
            }
        }
        #[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
        Err(ServiceError::HardwareError)
    }

    /// Set up the command, wait for not-busy, clear status, write
    /// the control and address registers.
    fn setup_command(
        &self,
        ctrl: FieldValue<u8, HSTCTL::Register>,
        xmitadd: u8,
    ) -> Result<(), ServiceError> {
        self.wait_not_busy()?;
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        {
            let regs = self.regs();
            let stat = regs.status().get();
            regs.status().set(stat);
            regs.control().write(ctrl);
            regs.xmit_addr().set(xmitadd);
        }
        Ok(())
    }

    /// Start the transaction and wait for completion.
    fn execute_and_complete(&self) -> Result<(), ServiceError> {
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        {
            let regs = self.regs();
            regs.control().modify(HSTCTL::START::SET);
            // Wait for the controller to signal activity.
            let mut loops = SMBUS_TIMEOUT;
            loop {
                let snapshot = regs.status().extract();
                let stat = snapshot.get();
                let completion = stat & !(HSTSTAT::HOST_BUSY::SET.value | STATUS_NON_COMPLETION);
                if completion != 0 && !snapshot.is_set(HSTSTAT::HOST_BUSY) {
                    if completion & STATUS_ERROR == 0 && snapshot.is_set(HSTSTAT::INTR) {
                        regs.status().set(stat);
                        return Ok(());
                    }
                    regs.status().set(stat);
                    // A device that does not acknowledge is an ordinary probe
                    // result (an empty DIMM slot, an absent clock generator),
                    // not a host-controller failure. Report it distinctly and
                    // let the caller decide what an absent device means.
                    if stat & STATUS_ERROR == HSTSTAT::DEV_ERR::SET.value {
                        fstart_log::debug!(
                            "i801-smbus: no device at {:#x}",
                            regs.xmit_addr().get() >> 1
                        );
                        return Err(ServiceError::NoDevice);
                    }
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
        #[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
        Err(ServiceError::HardwareError)
    }

    /// Read a byte via I801_BYTE_DATA command.
    pub fn read_byte_data(&self, addr: u8, cmd: u8) -> Result<u8, ServiceError> {
        self.setup_command(HSTCTL::TYPE::ByteData, xmit_read(addr))?;
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        {
            let regs = self.regs();
            regs.command().set(cmd);
            regs.data0().set(0);
            regs.data1().set(0);
        }
        self.execute_and_complete()?;
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        {
            Ok(self.regs().data0().get())
        }
        #[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
        Err(ServiceError::HardwareError)
    }

    /// Write a byte via I801_BYTE_DATA command.
    pub fn write_byte_data(&self, addr: u8, cmd: u8, val: u8) -> Result<(), ServiceError> {
        self.setup_command(HSTCTL::TYPE::ByteData, xmit_write(addr))?;
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        {
            let regs = self.regs();
            regs.command().set(cmd);
            regs.data0().set(val);
        }
        self.execute_and_complete()
    }

    /// Read a 16-bit word via I801_WORD_DATA command.
    pub fn read_word_data(&self, addr: u8, cmd: u8) -> Result<u16, ServiceError> {
        self.setup_command(HSTCTL::TYPE::WordData, xmit_read(addr))?;
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        {
            let regs = self.regs();
            regs.command().set(cmd);
            regs.data0().set(0);
            regs.data1().set(0);
        }
        self.execute_and_complete()?;
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        {
            let regs = self.regs();
            let lo = regs.data0().get();
            let hi = regs.data1().get();
            Ok((hi as u16) << 8 | lo as u16)
        }
        #[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
        Err(ServiceError::HardwareError)
    }

    /// Start a block transaction and service its byte engine.
    ///
    /// The controller raises BYTE_DONE for every byte it is ready to hand over
    /// or collect, and finishes the transaction itself once the device's byte
    /// count is reached. Bytes must be serviced as they are requested — waiting
    /// for completion first deadlocks the controller, which then keeps
    /// HOST_BUSY set across resets and wedges the whole bus.
    fn block_cmd_loop(
        &self,
        buf: &mut [u8],
        max_bytes: usize,
        mode: BlockMode,
    ) -> Result<usize, ServiceError> {
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        {
            let regs = self.regs();
            let write = mode == BlockMode::Write;
            match mode {
                // A write announces the byte count it will send; a read
                // starts from the count byte the device sends.
                BlockMode::Write => regs.data0().set(max_bytes as u8),
                BlockMode::Read => regs.data0().set(0),
                // DATA1 carries the EEPROM offset; the caller has set it.
                BlockMode::I2cRead => {}
            }
            // BYTE_DONE is raised before the host can be serviced, so the
            // first byte is loaded before the command is started.
            if write {
                regs.block_data().set(buf[0]);
            }
            // An I2C read ends with the byte the host NAKs; for a single
            // byte that is the first one.
            let last = if mode == BlockMode::I2cRead && max_bytes == 1 {
                HSTCTL::LAST_BYTE::SET
            } else {
                HSTCTL::LAST_BYTE::CLEAR
            };
            regs.control().modify(HSTCTL::START::SET + last);

            let mut bytes = 0usize;
            let mut loops = SMBUS_TIMEOUT;
            loop {
                let snapshot = regs.status().extract();
                let status = snapshot.get();
                if snapshot.is_set(HSTSTAT::BYTE_DONE) {
                    if write {
                        bytes += 1;
                        if bytes < max_bytes {
                            regs.block_data().set(buf[bytes]);
                        }
                    } else {
                        if bytes < max_bytes {
                            buf[bytes] = regs.block_data().get();
                        }
                        bytes += 1;
                        // Mark the next byte as the last one before
                        // releasing the engine to fetch it.
                        if mode == BlockMode::I2cRead && bytes + 1 >= max_bytes {
                            regs.control().modify(HSTCTL::LAST_BYTE::SET);
                        }
                    }
                    // Acknowledge the byte so the engine fetches the next one.
                    // Only this bit is written, as the controller expects.
                    regs.status().write(HSTSTAT::BYTE_DONE::SET);
                }
                let completion = status & !STATUS_NON_COMPLETION;
                if completion != 0 && !snapshot.is_set(HSTSTAT::HOST_BUSY) {
                    // W1C: acknowledge the observed snapshot, never modify().
                    regs.status().set(status);
                    // As for byte transactions, an unanswered address is a
                    // probe result (an empty slot), not a controller failure.
                    if completion & STATUS_ERROR == HSTSTAT::DEV_ERR::SET.value {
                        fstart_log::debug!(
                            "i801-smbus: no device at {:#x}",
                            regs.xmit_addr().get() >> 1
                        );
                        return Err(ServiceError::NoDevice);
                    }
                    if completion & STATUS_ERROR != 0 {
                        fstart_log::error!("i801-smbus: block error, status={:#x}", status);
                        return Err(ServiceError::HardwareError);
                    }
                    return Ok(bytes);
                }
                loops -= 1;
                if loops == 0 {
                    fstart_log::error!(
                        "i801-smbus: block transfer did not complete, status={:#x}",
                        status
                    );
                    return Err(ServiceError::Timeout);
                }
                core::hint::spin_loop();
            }
        }
        #[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
        {
            let _ = (buf, max_bytes, mode);
            Err(ServiceError::HardwareError)
        }
    }

    /// Read a block via I801_BLOCK_DATA (SMBus Block Read).
    ///
    /// `buf[0]` receives the device's count byte and the following bytes its
    /// data. Returns the number of bytes received, count byte included.
    pub fn read_block_data(
        &self,
        addr: u8,
        cmd: u8,
        buf: &mut [u8],
    ) -> Result<usize, ServiceError> {
        let max_bytes = buf.len().min(SMBUS_BLOCK_MAXLEN);
        if max_bytes == 0 {
            return Err(ServiceError::InvalidParam);
        }
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        {
            self.setup_command(HSTCTL::TYPE::BlockData, xmit_read(addr))?;
            self.regs().command().set(cmd);
            let moved = self.block_cmd_loop(&mut buf[..max_bytes], max_bytes, BlockMode::Read)?;
            // The device announces its length; a short read is a failed
            // transaction rather than a short block.
            let announced = self.regs().data0().get() as usize;
            if moved < announced {
                fstart_log::error!(
                    "i801-smbus: block read got {} of {} bytes",
                    moved as u32,
                    announced as u32
                );
                return Err(ServiceError::HardwareError);
            }
            Ok(moved)
        }
        #[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
        Err(ServiceError::HardwareError)
    }

    /// Write a block via I801_BLOCK_DATA (SMBus Block Write).
    ///
    /// `data[0]` is the count byte the device will see, followed by that many
    /// data bytes.
    pub fn write_block_data(&self, addr: u8, cmd: u8, data: &[u8]) -> Result<(), ServiceError> {
        if data.is_empty() || data.len() > SMBUS_BLOCK_MAXLEN {
            return Err(ServiceError::InvalidParam);
        }
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        {
            let mut scratch = [0u8; SMBUS_BLOCK_MAXLEN];
            scratch[..data.len()].copy_from_slice(data);
            self.setup_command(HSTCTL::TYPE::BlockData, xmit_write(addr))?;
            self.regs().command().set(cmd);
            let moved =
                self.block_cmd_loop(&mut scratch[..data.len()], data.len(), BlockMode::Write)?;
            if moved < data.len() {
                fstart_log::error!(
                    "i801-smbus: block write sent {} of {} bytes",
                    moved as u32,
                    data.len() as u32
                );
                return Err(ServiceError::HardwareError);
            }
        }
        Ok(())
    }

    /// Read `buf.len()` bytes from an I2C EEPROM starting at `offset`, in one
    /// I2C block read (ICH5 and later).
    ///
    /// coreboot `do_i2c_eeprom_read()`: the offset goes in DATA1, the address
    /// is sent with the write bit (the controller turns the direction around
    /// itself) and software NAKs the final byte. HOSTC.I2C_EN must be clear,
    /// which [`enable_on_i801`](Self::enable_on_i801) and the chipset
    /// drivers leave it.
    pub fn i2c_read(&self, addr: u8, offset: u8, buf: &mut [u8]) -> Result<(), ServiceError> {
        if buf.is_empty() || usize::from(offset) + buf.len() > 256 {
            return Err(ServiceError::InvalidParam);
        }
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        {
            self.setup_command(HSTCTL::TYPE::I2cBlockData, xmit_write(addr))?;
            self.regs().data1().set(offset);
            let len = buf.len();
            let moved = self.block_cmd_loop(buf, len, BlockMode::I2cRead)?;
            if moved < len {
                fstart_log::error!("i801-smbus: I2C read got {} of {} bytes", moved, len);
                return Err(ServiceError::HardwareError);
            }
            Ok(())
        }
        #[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
        Err(ServiceError::HardwareError)
    }

    /// Write a 16-bit word via I801_WORD_DATA command.
    pub fn write_word_data(&self, addr: u8, cmd: u8, val: u16) -> Result<(), ServiceError> {
        self.setup_command(HSTCTL::TYPE::WordData, xmit_write(addr))?;
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
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
    fn block_read(&mut self, addr: u8, cmd: u8, buf: &mut [u8]) -> Result<usize, ServiceError> {
        self.read_block_data(addr, cmd, buf)
    }
    fn block_write(&mut self, addr: u8, cmd: u8, data: &[u8]) -> Result<(), ServiceError> {
        self.write_block_data(addr, cmd, data)
    }
    fn i2c_eeprom_read(
        &mut self,
        addr: u8,
        offset: u8,
        buf: &mut [u8],
    ) -> Result<(), ServiceError> {
        self.i2c_read(addr, offset, buf)
    }
}
