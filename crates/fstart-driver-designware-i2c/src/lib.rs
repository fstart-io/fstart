//! DesignWare I2C driver.
//!
//! Implements I2C master mode using the DesignWare I2C block.
//! Provides [`Device`] and I2C master service (to be defined later).

#![no_std]

use fstart_mmio::MmioReadOnly;
use fstart_mmio::MmioReadWrite;
use tock_registers::interfaces::{Readable, Writeable};
use tock_registers::register_bitfields;
use tock_registers::register_structs;

use fstart_services::device::{Device, DeviceError};
use fstart_services::i2c::{ErrorKind, NoAcknowledgeSource};

register_bitfields! [u32,
    /// IC_CON — I2C Control Register
    IC_CON [
        /// Master mode enable
        MASTER_MODE OFFSET(0) NUMBITS(1) [],
        /// Speed mode: 01=standard, 10=fast, 11=high
        SPEED OFFSET(1) NUMBITS(2) [
            Standard = 0b01,
            Fast = 0b10,
            High = 0b11
        ],
        /// 10-bit address mode for slave
        IC_10BITADDR_SLAVE OFFSET(3) NUMBITS(1) [],
        /// 10-bit address mode for master
        IC_10BITADDR_MASTER OFFSET(4) NUMBITS(1) [],
        /// Restart enable
        IC_RESTART_EN OFFSET(5) NUMBITS(1) [],
        /// Slave disable
        IC_SLAVE_DISABLE OFFSET(6) NUMBITS(1) []
    ],

    /// IC_TAR — I2C Target Address Register
    IC_TAR [
        /// Target address (7-bit or 10-bit)
        IC_TAR_ADDR OFFSET(0) NUMBITS(10) []
    ],

    /// IC_DATA_CMD — I2C Data Buffer and Command Register
    IC_DATA_CMD [
        /// Data byte
        DAT OFFSET(0) NUMBITS(8) [],
        /// Command: 0=write, 1=read
        CMD OFFSET(8) NUMBITS(1) [
            Write = 0,
            Read = 1
        ],
        /// Generate STOP after this byte
        STOP OFFSET(9) NUMBITS(1) [],
        /// Generate RESTART before this byte
        RESTART OFFSET(10) NUMBITS(1) []
    ],

    /// IC_INTR_STAT / IC_RAW_INTR_STAT — Interrupt Status
    IC_INTR [
        RX_UNDER OFFSET(0) NUMBITS(1) [],
        RX_OVER OFFSET(1) NUMBITS(1) [],
        RX_FULL OFFSET(2) NUMBITS(1) [],
        TX_OVER OFFSET(3) NUMBITS(1) [],
        TX_EMPTY OFFSET(4) NUMBITS(1) [],
        RD_REQ OFFSET(5) NUMBITS(1) [],
        TX_ABRT OFFSET(6) NUMBITS(1) [],
        RX_DONE OFFSET(7) NUMBITS(1) [],
        ACTIVITY OFFSET(8) NUMBITS(1) [],
        STOP_DET OFFSET(9) NUMBITS(1) [],
        START_DET OFFSET(10) NUMBITS(1) [],
        GEN_CALL OFFSET(11) NUMBITS(1) []
    ],

    /// IC_ENABLE — I2C Enable Register
    IC_ENABLE [
        /// Module enable
        ENABLE OFFSET(0) NUMBITS(1) []
    ],

    /// IC_STATUS — I2C Status Register
    IC_STATUS [
        /// Controller activity
        ACTIVITY OFFSET(0) NUMBITS(1) [],
        /// Transmit FIFO not full
        TFNF OFFSET(1) NUMBITS(1) [],
        /// Transmit FIFO completely empty
        TFE OFFSET(2) NUMBITS(1) [],
        /// Receive FIFO not empty
        RFNE OFFSET(3) NUMBITS(1) [],
        /// Receive FIFO completely full
        RFF OFFSET(4) NUMBITS(1) [],
        /// Master FSM activity
        MST_ACTIVITY OFFSET(5) NUMBITS(1) []
    ]
];

register_structs! {
    /// DesignWare APB I2C register block.
    ///
    /// Offsets from U-Boot `struct i2c_regs` / coreboot `dw_i2c.c`.
    DesignwareI2cRegs {
        /// I2C Control Register
        (0x00 => pub ic_con: MmioReadWrite<u32, IC_CON::Register>),
        /// I2C Target Address Register
        (0x04 => pub ic_tar: MmioReadWrite<u32, IC_TAR::Register>),
        /// I2C Slave Address Register
        (0x08 => pub ic_sar: MmioReadWrite<u32>),
        /// I2C High Speed Master Mode Code Address
        (0x0C => pub ic_hs_maddr: MmioReadWrite<u32>),
        /// I2C Data Buffer and Command Register
        (0x10 => pub ic_data_cmd: MmioReadWrite<u32, IC_DATA_CMD::Register>),
        /// Standard Speed SCL High Count
        (0x14 => pub ic_ss_scl_hcnt: MmioReadWrite<u32>),
        /// Standard Speed SCL Low Count
        (0x18 => pub ic_ss_scl_lcnt: MmioReadWrite<u32>),
        /// Fast Speed SCL High Count
        (0x1C => pub ic_fs_scl_hcnt: MmioReadWrite<u32>),
        /// Fast Speed SCL Low Count
        (0x20 => pub ic_fs_scl_lcnt: MmioReadWrite<u32>),
        /// High Speed SCL High Count
        (0x24 => pub ic_hs_scl_hcnt: MmioReadWrite<u32>),
        /// High Speed SCL Low Count
        (0x28 => pub ic_hs_scl_lcnt: MmioReadWrite<u32>),
        /// Interrupt Status (read-only view of enabled interrupts)
        (0x2C => pub ic_intr_stat: MmioReadOnly<u32, IC_INTR::Register>),
        /// Interrupt Mask
        (0x30 => pub ic_intr_mask: MmioReadWrite<u32>),
        /// Raw Interrupt Status
        (0x34 => pub ic_raw_intr_stat: MmioReadOnly<u32, IC_INTR::Register>),
        /// Receive FIFO Threshold
        (0x38 => pub ic_rx_tl: MmioReadWrite<u32>),
        /// Transmit FIFO Threshold
        (0x3C => pub ic_tx_tl: MmioReadWrite<u32>),
        /// Clear Combined and Individual Interrupt
        (0x40 => pub ic_clr_intr: MmioReadOnly<u32>),
        /// Clear RX_UNDER
        (0x44 => pub ic_clr_rx_under: MmioReadOnly<u32>),
        /// Clear RX_OVER
        (0x48 => pub ic_clr_rx_over: MmioReadOnly<u32>),
        /// Clear TX_OVER
        (0x4C => pub ic_clr_tx_over: MmioReadOnly<u32>),
        /// Clear RD_REQ
        (0x50 => pub ic_clr_rd_req: MmioReadOnly<u32>),
        /// Clear TX_ABRT
        (0x54 => pub ic_clr_tx_abrt: MmioReadOnly<u32>),
        /// Clear RX_DONE
        (0x58 => pub ic_clr_rx_done: MmioReadOnly<u32>),
        /// Clear ACTIVITY
        (0x5C => pub ic_clr_activity: MmioReadOnly<u32>),
        /// Clear STOP_DET
        (0x60 => pub ic_clr_stop_det: MmioReadOnly<u32>),
        /// Clear START_DET
        (0x64 => pub ic_clr_start_det: MmioReadOnly<u32>),
        /// Clear GEN_CALL
        (0x68 => pub ic_clr_gen_call: MmioReadOnly<u32>),
        /// I2C Enable Register
        (0x6C => pub ic_enable: MmioReadWrite<u32, IC_ENABLE::Register>),
        /// I2C Status Register
        (0x70 => pub ic_status: MmioReadOnly<u32, IC_STATUS::Register>),
        /// I2C Transmit FIFO Level
        (0x74 => pub ic_txflr: MmioReadOnly<u32>),
        /// I2C Receive FIFO Level
        (0x78 => pub ic_rxflr: MmioReadOnly<u32>),
        /// I2C SDA Hold Time
        (0x7C => pub ic_sda_hold: MmioReadWrite<u32>),
        /// I2C Transmit Abort Source
        (0x80 => pub ic_tx_abrt_source: MmioReadOnly<u32>),
        (0x84 => _reserved0),
        /// I2C Enable Status Register
        (0x9C => pub ic_enable_status: MmioReadOnly<u32>),
        (0xA0 => @END),
    }
}

// ---------------------------------------------------------------------------
// Driver config and struct
// ---------------------------------------------------------------------------

/// I2C bus speed modes, matching the DesignWare IC_CON.SPEED field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum I2cSpeed {
    /// 100 kHz standard mode
    Standard,
    /// 400 kHz fast mode
    Fast,
}

/// Typed configuration for the DesignWare I2C driver.
///
/// Contains exactly the fields this driver needs.
/// Serializable with both RON (build-time validation) and postcard
/// (runtime config from FFS).
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct DesignwareI2cConfig {
    /// MMIO base address of the register block.
    pub base_addr: u64,
    /// Input clock frequency in Hz (IC_CLK).
    pub clock_freq: u32,
    /// Desired bus speed.
    pub bus_speed: I2cSpeed,
}

/// DesignWare APB I2C controller driver.
///
/// Operates in 7-bit addressing master mode. Uses polled I/O
/// (spin-waits on status register bits).
///
/// Implements [`embedded_hal::i2c::I2c`] for ecosystem compatibility.
pub struct DesignwareI2c {
    regs: &'static DesignwareI2cRegs,
    clock_freq: u32,
    bus_speed: I2cSpeed,
}

// SAFETY: MMIO registers are hardware-fixed addresses; access is safe
// as long as the base address is correct (which comes from the board RON).
unsafe impl Send for DesignwareI2c {}
unsafe impl Sync for DesignwareI2c {}

/// Maximum spin iterations before declaring a timeout.
/// In firmware we don't have a real timer yet, so we use a generous
/// iteration count. At ~1ns per iteration on a fast core, 1M iterations
/// gives ~1ms which is plenty for I2C FIFO operations.
const TIMEOUT_ITERS: u32 = 1_000_000;

impl Device for DesignwareI2c {
    const NAME: &'static str = "designware-i2c";
    const COMPATIBLE: &'static [&'static str] = &["snps,designware-i2c", "dw-apb-i2c"];
    type Config = DesignwareI2cConfig;

    fn new(config: &DesignwareI2cConfig) -> Result<Self, DeviceError> {
        Ok(Self {
            // SAFETY: base_addr comes from the board RON and is validated
            // by codegen at build time.
            regs: unsafe { &*(config.base_addr as *const DesignwareI2cRegs) },
            clock_freq: config.clock_freq,
            bus_speed: config.bus_speed,
        })
    }

    fn init(&self) -> Result<(), DeviceError> {
        // Disable controller before configuring
        self.regs.ic_enable.write(IC_ENABLE::ENABLE::CLEAR);
        self.wait_disabled()?;

        // Configure as master, 7-bit addressing, restart enabled, slave disabled
        let speed = match self.bus_speed {
            I2cSpeed::Standard => IC_CON::SPEED::Standard,
            I2cSpeed::Fast => IC_CON::SPEED::Fast,
        };
        self.regs.ic_con.write(
            IC_CON::MASTER_MODE::SET
                + speed
                + IC_CON::IC_RESTART_EN::SET
                + IC_CON::IC_SLAVE_DISABLE::SET,
        );

        // Set SCL timing based on clock frequency and speed mode.
        // These formulas come from the DesignWare databook / U-Boot driver.
        self.set_scl_timing();

        // Set FIFO thresholds: RX trigger at 0 (every byte), TX at 0
        self.regs.ic_rx_tl.set(0);
        self.regs.ic_tx_tl.set(0);

        // Mask all interrupts (we use polled I/O)
        self.regs.ic_intr_mask.set(0);

        // Enable controller
        self.regs.ic_enable.write(IC_ENABLE::ENABLE::SET);

        Ok(())
    }
}

impl DesignwareI2c {
    /// Wait for the controller to become disabled.
    fn wait_disabled(&self) -> Result<(), DeviceError> {
        for _ in 0..TIMEOUT_ITERS {
            if self.regs.ic_enable_status.get() & 0x1 == 0 {
                return Ok(());
            }
            core::hint::spin_loop();
        }
        Err(DeviceError::InitFailed)
    }

    /// Set SCL high/low count registers based on clock frequency and speed.
    ///
    /// Formulas based on the DesignWare databook:
    ///   hcnt = IC_CLK_FREQ / (2 * bus_speed) - 8
    ///   lcnt = IC_CLK_FREQ / (2 * bus_speed) - 1
    ///
    /// The constants approximate the minimum SCL high/low times from the
    /// I2C specification, adjusted for the controller's internal delays.
    fn set_scl_timing(&self) {
        let clk = self.clock_freq as u64;

        match self.bus_speed {
            I2cSpeed::Standard => {
                // Standard mode: 100 kHz
                // Min high period: 4.0us, min low period: 4.7us
                let hcnt = (clk * 40 / 10_000_000).saturating_sub(8) as u32;
                let lcnt = (clk * 47 / 10_000_000).saturating_sub(1) as u32;
                self.regs.ic_ss_scl_hcnt.set(hcnt.max(6));
                self.regs.ic_ss_scl_lcnt.set(lcnt.max(8));
            }
            I2cSpeed::Fast => {
                // Fast mode: 400 kHz
                // Min high period: 0.6us, min low period: 1.3us
                let hcnt = (clk * 6 / 10_000_000).saturating_sub(8) as u32;
                let lcnt = (clk * 13 / 10_000_000).saturating_sub(1) as u32;
                self.regs.ic_fs_scl_hcnt.set(hcnt.max(6));
                self.regs.ic_fs_scl_lcnt.set(lcnt.max(8));
            }
        }

        // SDA hold time: 300ns for standard/fast mode
        // hold_count = IC_CLK_FREQ * 300 / 1_000_000_000
        let sda_hold = (clk * 300 / 1_000_000_000).max(1) as u32;
        self.regs.ic_sda_hold.set(sda_hold);
    }

    /// Wait for the TX FIFO to have room (TFNF = TX FIFO Not Full).
    fn wait_tx_ready(&self) -> Result<(), ErrorKind> {
        for _ in 0..TIMEOUT_ITERS {
            if self.regs.ic_status.is_set(IC_STATUS::TFNF) {
                return Ok(());
            }
            // Check for TX abort
            if self.regs.ic_raw_intr_stat.is_set(IC_INTR::TX_ABRT) {
                // Clear abort
                let _ = self.regs.ic_clr_tx_abrt.get();
                return Err(ErrorKind::NoAcknowledge(NoAcknowledgeSource::Unknown));
            }
            core::hint::spin_loop();
        }
        Err(ErrorKind::Bus)
    }

    /// Wait for data in the RX FIFO (RFNE = RX FIFO Not Empty).
    fn wait_rx_ready(&self) -> Result<(), ErrorKind> {
        for _ in 0..TIMEOUT_ITERS {
            if self.regs.ic_status.is_set(IC_STATUS::RFNE) {
                return Ok(());
            }
            // Check for TX abort (can happen during read address phase)
            if self.regs.ic_raw_intr_stat.is_set(IC_INTR::TX_ABRT) {
                let _ = self.regs.ic_clr_tx_abrt.get();
                return Err(ErrorKind::NoAcknowledge(NoAcknowledgeSource::Unknown));
            }
            core::hint::spin_loop();
        }
        Err(ErrorKind::Bus)
    }

    /// Set the target slave address. Must be done while controller is enabled.
    ///
    /// The DesignWare controller requires disabling and re-enabling to change
    /// the target address. For simplicity we always reconfigure.
    fn set_target_addr(&self, addr: u8) -> Result<(), ErrorKind> {
        // Disable to change target
        self.regs.ic_enable.write(IC_ENABLE::ENABLE::CLEAR);
        let mut disabled = false;
        for _ in 0..TIMEOUT_ITERS {
            if self.regs.ic_enable_status.get() & 0x1 == 0 {
                disabled = true;
                break;
            }
            core::hint::spin_loop();
        }
        if !disabled {
            return Err(ErrorKind::Bus);
        }

        self.regs.ic_tar.write(IC_TAR::IC_TAR_ADDR.val(addr as u32));

        // Re-enable
        self.regs.ic_enable.write(IC_ENABLE::ENABLE::SET);

        // Clear any pending interrupts from the address change
        let _ = self.regs.ic_clr_intr.get();

        Ok(())
    }

    /// Write bytes to the bus. If `send_stop` is true, the last byte
    /// gets a STOP condition. An empty write with `send_stop` issues a
    /// zero-length read with STOP to terminate the transaction cleanly.
    fn write_bytes(&self, data: &[u8], send_stop: bool) -> Result<(), ErrorKind> {
        if data.is_empty() {
            if send_stop {
                // No data to write, but we need a STOP on the bus.
                // Issue a single read-with-STOP so the controller generates
                // a STOP condition, then discard the byte.
                self.wait_tx_ready()?;
                self.regs
                    .ic_data_cmd
                    .write(IC_DATA_CMD::CMD::Read + IC_DATA_CMD::STOP::SET);
                self.wait_rx_ready()?;
                let _ = self.regs.ic_data_cmd.get();
            }
            return Ok(());
        }

        let last = data.len() - 1;
        for (i, &byte) in data.iter().enumerate() {
            self.wait_tx_ready()?;
            if i == last && send_stop {
                self.regs.ic_data_cmd.write(
                    IC_DATA_CMD::DAT.val(byte as u32)
                        + IC_DATA_CMD::CMD::Write
                        + IC_DATA_CMD::STOP::SET,
                );
            } else {
                self.regs
                    .ic_data_cmd
                    .write(IC_DATA_CMD::DAT.val(byte as u32) + IC_DATA_CMD::CMD::Write);
            }
        }
        Ok(())
    }

    /// Issue read commands and collect bytes from the bus. If `send_stop`
    /// is true, the last read gets a STOP condition. An empty read with
    /// `send_stop` issues a dummy read-with-STOP to terminate the
    /// transaction cleanly.
    fn read_bytes(&self, buf: &mut [u8], send_stop: bool) -> Result<(), ErrorKind> {
        if buf.is_empty() {
            if send_stop {
                // No data to read, but we need a STOP on the bus.
                self.wait_tx_ready()?;
                self.regs
                    .ic_data_cmd
                    .write(IC_DATA_CMD::CMD::Read + IC_DATA_CMD::STOP::SET);
                self.wait_rx_ready()?;
                let _ = self.regs.ic_data_cmd.get();
            }
            return Ok(());
        }

        let last = buf.len() - 1;

        // Issue read commands for each byte
        for i in 0..buf.len() {
            self.wait_tx_ready()?;
            if i == last && send_stop {
                self.regs
                    .ic_data_cmd
                    .write(IC_DATA_CMD::CMD::Read + IC_DATA_CMD::STOP::SET);
            } else {
                self.regs.ic_data_cmd.write(IC_DATA_CMD::CMD::Read);
            }
        }

        // Read received data
        for byte in buf.iter_mut() {
            self.wait_rx_ready()?;
            *byte = (self.regs.ic_data_cmd.get() & 0xFF) as u8;
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// embedded-hal I2C implementation
// ---------------------------------------------------------------------------

impl embedded_hal::i2c::ErrorType for DesignwareI2c {
    type Error = embedded_hal::i2c::ErrorKind;
}

impl embedded_hal::i2c::I2c for DesignwareI2c {
    fn transaction(
        &mut self,
        address: u8,
        operations: &mut [embedded_hal::i2c::Operation<'_>],
    ) -> Result<(), Self::Error> {
        if operations.is_empty() {
            return Ok(());
        }

        self.set_target_addr(address)?;

        let last_idx = operations.len() - 1;
        for (i, op) in operations.iter_mut().enumerate() {
            let send_stop = i == last_idx;
            match op {
                embedded_hal::i2c::Operation::Write(data) => self.write_bytes(data, send_stop)?,
                embedded_hal::i2c::Operation::Read(buf) => self.read_bytes(buf, send_stop)?,
            }
        }

        Ok(())
    }
}
