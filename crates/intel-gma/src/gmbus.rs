//! GMBUS/DDC helpers for Intel GMA display initialization.
//!
//! libgfxinit probes EDID over GMBUS/DDC before choosing EDID-backed modes.
//! fstart performs the legacy GMCH/PCH GMBUS transaction needed to read an EDID
//! base block for VGA/LVDS/HDMI-style DDC pins. DisplayPort/eDP AUX probing is
//! handled by the DP AUX path.

use heapless::Vec;

use crate::edid::{self, EDID_BLOCK_LEN, Edid, MAX_EDID_MODES, MAX_EXTENSION_BLOCKS};
use crate::error::GmaError;
use crate::mmio::Mmio;
use crate::types::{PhysAddr, Port};

use serde::{Deserialize, Serialize};
use tock_registers::register_bitfields;
use tock_registers::register_structs;
use tock_registers::registers::ReadWrite;

/// Standard 7-bit DDC EDID I2C address.
pub const DDC_EDID_ADDRESS: u8 = 0x50;

/// GMCH GMBUS register base offset in the display MMIO BAR.
pub const GMCH_GMBUS_BASE_OFFSET: usize = 0x5100;

/// PCH GMBUS register base offset in the display MMIO BAR for split-PCH generations.
pub const PCH_GMBUS_BASE_OFFSET: usize = 0x0c_5100;

register_bitfields! [u32,
    /// GMBUS0 — clock rate and pin-pair select.
    pub GMBUS0_REG [
        /// Use HDCP AKSV source instead of GMBUS3 data.
        AKSV_SELECT OFFSET(11) NUMBITS(1) [],
        /// GMBUS clock rate selector.
        RATE OFFSET(8) NUMBITS(3) [],
        /// Extended 300 ns hold time; reserved on Pineview.
        HOLD_EXT OFFSET(7) NUMBITS(1) [],
        /// Byte-count override used by long transfers.
        BYTE_COUNT_OVERRIDE OFFSET(6) NUMBITS(1) [],
        /// Selected GMBUS/DDC pin pair.
        PIN_SELECT OFFSET(0) NUMBITS(4) []
    ],

    /// GMBUS1 — command and transfer status control.
    pub GMBUS1_REG [
        /// Software clear interrupt/error latch.
        SW_CLR_INT OFFSET(31) NUMBITS(1) [],
        /// Software ready/request bit.
        SW_RDY OFFSET(30) NUMBITS(1) [],
        /// Enable hardware timeout handling.
        TIMEOUT_EN OFFSET(29) NUMBITS(1) [],
        /// Bus cycle select.
        CYCLE OFFSET(25) NUMBITS(3) [],
        /// Total transfer byte count.
        BYTE_COUNT OFFSET(16) NUMBITS(9) [],
        /// 8-bit slave/register index.
        SLAVE_INDEX OFFSET(8) NUMBITS(8) [],
        /// 7-bit I2C slave address.
        SLAVE_ADDR OFFSET(1) NUMBITS(7) [],
        /// Transfer direction; 0 = write, 1 = read.
        DIRECTION OFFSET(0) NUMBITS(1) []
    ],

    /// GMBUS2 — hardware status.
    pub GMBUS2_REG [
        /// GMBUS is in use by hardware.
        INUSE OFFSET(15) NUMBITS(1) [],
        /// Hardware wait phase is active.
        HW_WAIT_PHASE OFFSET(14) NUMBITS(1) [],
        /// Slave stall timeout error.
        STALL_TIMEOUT OFFSET(13) NUMBITS(1) [],
        /// Interrupt status.
        INT_STATUS OFFSET(12) NUMBITS(1) [],
        /// Hardware data ready.
        HW_RDY OFFSET(11) NUMBITS(1) [],
        /// Target NAK indicator.
        NAK OFFSET(10) NUMBITS(1) [],
        /// GMBUS transfer is active.
        ACTIVE OFFSET(9) NUMBITS(1) [],
        /// Current byte count.
        BYTE_COUNT OFFSET(0) NUMBITS(9) []
    ],

    /// GMBUS4 — interrupt mask.
    pub GMBUS4_REG [
        /// Slave timeout interrupt enable.
        SLAVE_TIMEOUT_EN OFFSET(4) NUMBITS(1) [],
        /// NAK interrupt enable.
        NAK_EN OFFSET(3) NUMBITS(1) [],
        /// Idle interrupt enable.
        IDLE_EN OFFSET(2) NUMBITS(1) [],
        /// Hardware wait interrupt enable.
        HW_WAIT_EN OFFSET(1) NUMBITS(1) [],
        /// Hardware ready interrupt enable.
        HW_RDY_EN OFFSET(0) NUMBITS(1) []
    ],

    /// GMBUS5 — two-byte index control.
    pub GMBUS5_REG [
        /// Enable two-byte index mode.
        INDEX_2BYTE_EN OFFSET(31) NUMBITS(1) [],
        /// 16-bit slave/register index value.
        INDEX OFFSET(0) NUMBITS(16) []
    ]
];

register_structs! {
    /// Typed GMBUS register window relative to either GMCH or PCH GMBUS base.
    pub GmbusRegs {
        /// GMBUS0 clock/pin select register.
        (0x00 => pub gmbus0: ReadWrite<u32, GMBUS0_REG::Register>),
        /// GMBUS1 command register.
        (0x04 => pub gmbus1: ReadWrite<u32, GMBUS1_REG::Register>),
        /// GMBUS2 status register.
        (0x08 => pub gmbus2: ReadWrite<u32, GMBUS2_REG::Register>),
        /// GMBUS3 data register.
        (0x0c => pub gmbus3: ReadWrite<u32>),
        /// GMBUS4 interrupt mask register.
        (0x10 => pub gmbus4: ReadWrite<u32, GMBUS4_REG::Register>),
        (0x14 => _reserved0),
        /// GMBUS5 extended index register.
        (0x20 => pub gmbus5: ReadWrite<u32, GMBUS5_REG::Register>),
        (0x24 => @END),
    }
}

/// Logical GMBUS pin pair used for DDC on a connector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GmbusPin {
    /// Analog/VGA DDC pins.
    Analog,
    /// LVDS panel DDC pins.
    Panel,
    /// HDMI B DDC pins.
    DigitalB,
    /// HDMI C DDC pins.
    DigitalC,
    /// HDMI D DDC pins.
    DigitalD,
}

impl GmbusPin {
    /// Linux/libgfxinit-compatible legacy GMBUS0 pin-pair select value.
    pub const fn legacy_select(self) -> u8 {
        match self {
            Self::Analog => 2,
            Self::Panel => 3,
            // The historical pin order is C, B, D: DPC=4, DPB=5, DPD=6.
            Self::DigitalB => 5,
            Self::DigitalC => 4,
            Self::DigitalD => 6,
        }
    }

    /// Convert a legacy VBT/Linux GMBUS pin-select value to a modeled pin.
    pub const fn from_legacy_select(select: u8) -> Option<Self> {
        match select {
            2 => Some(Self::Analog),
            3 => Some(Self::Panel),
            4 => Some(Self::DigitalC),
            5 => Some(Self::DigitalB),
            6 => Some(Self::DigitalD),
            _ => None,
        }
    }
}

/// GMBUS clock rate selector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GmbusRate {
    /// 100 kHz, Linux's conservative default.
    Khz100,
    /// 50 kHz.
    Khz50,
    /// 400 kHz; reserved on Pineview.
    Khz400,
    /// 1 MHz; reserved on Pineview.
    Mhz1,
}

impl GmbusRate {
    /// Raw GMBUS0 rate selector value.
    pub const fn select(self) -> u8 {
        match self {
            Self::Khz100 => 0,
            Self::Khz50 => 1,
            Self::Khz400 => 2,
            Self::Mhz1 => 3,
        }
    }
}

/// GMBUS command cycle selector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GmbusCycle {
    /// Stop cycle.
    Stop,
    /// Indexed transfer cycle.
    Index,
    /// Wait/data cycle.
    Wait,
    /// Combined indexed wait/data cycle used by Linux/libgfxinit for one-byte-indexed reads.
    IndexWait,
}

impl GmbusCycle {
    /// Raw GMBUS1 cycle selector value.
    pub const fn select(self) -> u8 {
        match self {
            Self::Stop => 0b100,
            Self::Index => 0b010,
            Self::Wait => 0b001,
            Self::IndexWait => 0b011,
        }
    }
}

/// GMBUS transfer direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GmbusDirection {
    /// Write to the I2C slave.
    Write,
    /// Read from the I2C slave.
    Read,
}

impl GmbusDirection {
    /// Raw GMBUS1 direction bit.
    pub const fn bit(self) -> u8 {
        match self {
            Self::Write => 0,
            Self::Read => 1,
        }
    }
}

/// Data-only GMBUS0 programming value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gmbus0Config {
    /// Selected pin pair.
    pub pin: GmbusPin,
    /// Bus clock rate.
    pub rate: GmbusRate,
    /// Enable extended hold timing; reserved on Pineview.
    pub hold_ext: bool,
    /// Enable byte-count override for long transfers.
    pub byte_count_override: bool,
}

impl Gmbus0Config {
    /// Conservative 100 kHz pin selection used by Linux and libgfxinit.
    pub const fn conservative(pin: GmbusPin) -> Self {
        Self {
            pin,
            rate: GmbusRate::Khz100,
            hold_ext: false,
            byte_count_override: false,
        }
    }

    /// Encode the value to write to GMBUS0.
    pub const fn encode(self) -> u32 {
        let hold = if self.hold_ext { 1u32 << 7 } else { 0 };
        let override_bit = if self.byte_count_override {
            1u32 << 6
        } else {
            0
        };
        (self.pin.legacy_select() as u32) | ((self.rate.select() as u32) << 8) | hold | override_bit
    }
}

/// Data-only GMBUS1 command encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GmbusCommand {
    // Keep fields private so safe callers cannot bypass `new()` and create
    // values that overflow the fixed-width hardware fields encoded below.
    cycle: GmbusCycle,
    byte_count: u16,
    slave_address: u8,
    index: u8,
    direction: GmbusDirection,
    enable_timeout: bool,
    software_ready: bool,
}

impl GmbusCommand {
    /// Build a checked command model.
    pub const fn new(
        cycle: GmbusCycle,
        byte_count: u16,
        slave_address: u8,
        index: u8,
        direction: GmbusDirection,
    ) -> Result<Self, GmaError> {
        if byte_count > 128 || slave_address > 0x7f {
            return Err(GmaError::InvalidConfig);
        }
        Ok(Self {
            cycle,
            byte_count,
            slave_address,
            index,
            direction,
            enable_timeout: false,
            software_ready: true,
        })
    }

    /// Bus cycle selector.
    pub const fn cycle(self) -> GmbusCycle {
        self.cycle
    }

    /// Total transfer byte count, limited by the 9-bit hardware field.
    ///
    /// Generation-specific chunking and live-transfer limits are deferred until
    /// hardware GMBUS transactions are enabled.
    pub const fn byte_count(self) -> u16 {
        self.byte_count
    }

    /// 7-bit I2C slave address.
    pub const fn slave_address(self) -> u8 {
        self.slave_address
    }

    /// Optional 8-bit register/index value.
    pub const fn index(self) -> u8 {
        self.index
    }

    /// Transfer direction.
    pub const fn direction(self) -> GmbusDirection {
        self.direction
    }

    /// Hardware timeout handling state.
    pub const fn timeout_enabled(self) -> bool {
        self.enable_timeout
    }

    /// Software-ready bit state.
    pub const fn software_ready(self) -> bool {
        self.software_ready
    }

    /// Encode the value to write to GMBUS1.
    pub const fn encode(self) -> u32 {
        let timeout = if self.enable_timeout { 1u32 << 29 } else { 0 };
        let ready = if self.software_ready { 1u32 << 30 } else { 0 };
        ready
            | timeout
            | ((self.cycle.select() as u32) << 25)
            | ((self.byte_count as u32) << 16)
            | ((self.index as u32) << 8)
            | ((self.slave_address as u32) << 1)
            | (self.direction.bit() as u32)
    }

    /// Linux-style STOP command used to terminate a transaction.
    pub const fn stop() -> Self {
        Self {
            cycle: GmbusCycle::Stop,
            byte_count: 0,
            slave_address: 0,
            index: 0,
            direction: GmbusDirection::Write,
            enable_timeout: false,
            software_ready: true,
        }
    }
}

/// Decoded GMBUS2 status bits used by a future transaction state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GmbusStatus {
    /// Hardware reports bus ownership/in-use.
    pub in_use: bool,
    /// Hardware wait phase is active.
    pub wait_phase: bool,
    /// Slave stall timeout was reported.
    pub stall_timeout: bool,
    /// Interrupt status is pending.
    pub interrupt: bool,
    /// Hardware data ready.
    pub hardware_ready: bool,
    /// Target NAK was reported.
    pub nak: bool,
    /// Bus transaction is active.
    pub active: bool,
    /// Current byte count.
    pub byte_count: u16,
}

impl GmbusStatus {
    /// Decode raw GMBUS2 bits.
    pub const fn from_bits(bits: u32) -> Self {
        Self {
            in_use: bits & (1 << 15) != 0,
            wait_phase: bits & (1 << 14) != 0,
            stall_timeout: bits & (1 << 13) != 0,
            interrupt: bits & (1 << 12) != 0,
            hardware_ready: bits & (1 << 11) != 0,
            nak: bits & (1 << 10) != 0,
            active: bits & (1 << 9) != 0,
            byte_count: (bits & 0x01ff) as u16,
        }
    }

    /// Status has an error that requires clearing/reset before retry.
    pub const fn has_error(self) -> bool {
        self.stall_timeout || self.nak
    }

    /// Status can satisfy a poll for transaction completion: the cycle finished
    /// or an error was raised. Data is only valid once `hardware_ready` is set.
    pub const fn is_wait_complete(self) -> bool {
        !self.active || self.has_error()
    }

    /// Status reports a 4-byte word available in GMBUS3.
    pub const fn is_data_ready(self) -> bool {
        self.hardware_ready || self.has_error()
    }

    /// The bus is fully idle after a STOP/reset sequence.
    pub const fn is_idle(self) -> bool {
        !self.active && !self.in_use && !self.wait_phase
    }
}

/// One modeled register action in a future GMBUS transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GmbusPlanStep {
    /// Program GMBUS0 with the given value.
    SelectPin(u32),
    /// Program GMBUS1 with a command value.
    Command(u32),
    /// Poll GMBUS2 for completion/error.
    PollStatus,
    /// Read data bytes from GMBUS3.
    ReadData { bytes: u16 },
    /// Disable GMBUS0 after idle.
    Disable,
}

/// Small, fixed plan for one GMBUS read transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GmbusReadPlan {
    /// Ordered modeled steps; entries after `len` are padding.
    pub steps: [GmbusPlanStep; 6],
    /// Number of valid steps.
    pub len: usize,
}

impl GmbusReadPlan {
    /// Model an indexed read like EDID block 0 without touching hardware.
    pub const fn indexed_read(
        pin: GmbusPin,
        address: u8,
        index: u8,
        bytes: u16,
    ) -> Result<Self, GmaError> {
        let command = match GmbusCommand::new(
            GmbusCycle::IndexWait,
            bytes,
            address,
            index,
            GmbusDirection::Read,
        ) {
            Ok(command) => command,
            Err(error) => return Err(error),
        };
        Ok(Self {
            steps: [
                GmbusPlanStep::SelectPin(Gmbus0Config::conservative(pin).encode()),
                GmbusPlanStep::Command(command.encode()),
                GmbusPlanStep::PollStatus,
                GmbusPlanStep::ReadData { bytes },
                GmbusPlanStep::Command(GmbusCommand::stop().encode()),
                GmbusPlanStep::Disable,
            ],
            len: 6,
        })
    }
}

/// Data-only connector probe candidate for GMBUS/DDC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DdcProbeCandidate {
    /// Logical port being probed.
    pub port: Port,
    /// GMBUS pin pair to try for this port.
    pub pin: GmbusPin,
}

/// Data-only libgfxinit-style DDC probe order for GMBUS-backed ports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DdcProbePlan {
    /// Candidate entries; entries after `len` are padding.
    pub candidates: [DdcProbeCandidate; 5],
    /// Number of valid candidates.
    pub len: usize,
}

impl DdcProbePlan {
    /// Build a single-port DDC probe plan.
    pub const fn for_port(port: Port) -> Result<Self, GmaError> {
        match ddc_pin_for_port(port) {
            Some(pin) => Ok(Self {
                candidates: [DdcProbeCandidate { port, pin }; 5],
                len: 1,
            }),
            None => Err(GmaError::UnsupportedPort),
        }
    }

    /// Build the legacy all-GMBUS probe order used when board policy does not
    /// name one connector: VGA, LVDS panel, then HDMI/DVI digital pins B/C/D.
    pub const fn legacy_gmbus_order() -> Self {
        Self {
            candidates: [
                DdcProbeCandidate {
                    port: Port::Vga,
                    pin: GmbusPin::Analog,
                },
                DdcProbeCandidate {
                    port: Port::Lvds,
                    pin: GmbusPin::Panel,
                },
                DdcProbeCandidate {
                    port: Port::HdmiA,
                    pin: GmbusPin::DigitalB,
                },
                DdcProbeCandidate {
                    port: Port::HdmiB,
                    pin: GmbusPin::DigitalC,
                },
                DdcProbeCandidate {
                    port: Port::HdmiC,
                    pin: GmbusPin::DigitalD,
                },
            ],
            len: 5,
        }
    }

    /// Return a candidate by index.
    pub const fn candidate(self, index: usize) -> Option<DdcProbeCandidate> {
        if index < self.len {
            Some(self.candidates[index])
        } else {
            None
        }
    }
}

/// Resolve the conventional GMBUS DDC pin group for ports that use GMBUS today.
pub const fn ddc_pin_for_port(port: Port) -> Option<GmbusPin> {
    match port {
        Port::Vga => Some(GmbusPin::Analog),
        Port::Lvds => Some(GmbusPin::Panel),
        Port::HdmiA => Some(GmbusPin::DigitalB),
        Port::HdmiB => Some(GmbusPin::DigitalC),
        Port::HdmiC => Some(GmbusPin::DigitalD),
        Port::DpA | Port::DpB | Port::DpC | Port::DpD | Port::Edp => None,
    }
}

/// Abstract DDC reader for future generation-specific GMBUS implementations.
pub trait DdcBus {
    /// Read one EDID block from the connector DDC address.
    fn read_edid_block(
        &mut self,
        address: u8,
        block_index: u8,
        block: &mut [u8; EDID_BLOCK_LEN],
    ) -> Result<(), GmaError>;
}

/// Hardware GMBUS DDC reader for legacy MMIO-backed Intel display engines.
#[derive(Debug, Clone, Copy)]
pub struct HardwareGmbus {
    mmio: Mmio,
    base_offset: usize,
    pin: GmbusPin,
}

impl HardwareGmbus {
    /// Create a GMBUS reader using the GMCH GMBUS register block.
    ///
    /// # Safety
    ///
    /// `gtt_mmio_base` must point at a valid, mapped Intel GMA display MMIO BAR.
    pub const unsafe fn gmch(gtt_mmio_base: PhysAddr, pin: GmbusPin) -> Self {
        Self {
            // SAFETY: forwarded to the caller contract above.
            mmio: unsafe { Mmio::new(gtt_mmio_base) },
            base_offset: GMCH_GMBUS_BASE_OFFSET,
            pin,
        }
    }

    /// Create a GMBUS reader using the split-PCH GMBUS register block.
    ///
    /// # Safety
    ///
    /// `gtt_mmio_base` must point at a valid, mapped Intel GMA display MMIO BAR.
    pub const unsafe fn pch(gtt_mmio_base: PhysAddr, pin: GmbusPin) -> Self {
        Self {
            // SAFETY: forwarded to the caller contract above.
            mmio: unsafe { Mmio::new(gtt_mmio_base) },
            base_offset: PCH_GMBUS_BASE_OFFSET,
            pin,
        }
    }

    fn reg(&self, offset: usize) -> usize {
        self.base_offset + offset
    }

    fn status(&self) -> GmbusStatus {
        GmbusStatus::from_bits(self.mmio.read32(self.reg(0x08)))
    }

    fn wait_complete(&self) -> Result<GmbusStatus, GmaError> {
        let mut timeout = 100_000;
        while timeout != 0 {
            let status = self.status();
            if status.is_wait_complete() {
                if status.has_error() {
                    return Err(GmaError::HardwareError);
                }
                return Ok(status);
            }
            timeout -= 1;
            core::hint::spin_loop();
        }
        Err(GmaError::Timeout)
    }

    fn stop(&self) -> Result<(), GmaError> {
        self.mmio
            .write32(self.reg(0x04), GmbusCommand::stop().encode());
        self.mmio.posting_read(self.reg(0x04));
        let mut timeout = 100_000;
        while timeout != 0 {
            if self.status().is_idle() {
                // Release ownership (libgfxinit `Release_GMBUS` sets GMBUS2
                // INUSE) before disabling the pin selection.
                self.mmio.set_bits32(self.reg(0x08), 1 << 15);
                self.mmio.write32(self.reg(0x00), 0);
                return Ok(());
            }
            timeout -= 1;
            core::hint::spin_loop();
        }
        self.mmio.set_bits32(self.reg(0x08), 1 << 15);
        self.mmio.write32(self.reg(0x00), 0);
        Err(GmaError::Timeout)
    }

    /// Take ownership of the bus after a stale transfer (libgfxinit
    /// `Wait_Unset_Mask (GMBUS2_INUSE)` plus `Check_And_Reset`).
    fn acquire(&self) -> Result<(), GmaError> {
        self.mmio.write32(self.reg(0x00), 0);
        self.mmio.write32(self.reg(0x04), 0);
        let mut timeout = 100_000;
        while timeout != 0 {
            if !self.status().in_use {
                return Ok(());
            }
            timeout -= 1;
            core::hint::spin_loop();
        }
        Err(GmaError::Timeout)
    }

    /// Wait for the 4-byte word in GMBUS3 to become valid.
    fn wait_data_ready(&self) -> Result<(), GmaError> {
        let mut timeout = 100_000;
        while timeout != 0 {
            let status = self.status();
            if status.has_error() {
                return Err(GmaError::HardwareError);
            }
            if status.is_data_ready() {
                return Ok(());
            }
            timeout -= 1;
            core::hint::spin_loop();
        }
        Err(GmaError::Timeout)
    }

    /// Write one byte to a slave register (E-DDC segment pointer).
    fn write_byte(&self, address: u8, value: u8) -> Result<(), GmaError> {
        let command = GmbusCommand::new(
            GmbusCycle::IndexWait,
            1,
            address,
            0,
            GmbusDirection::Write,
        )?;
        self.mmio.write32(self.reg(0x04), command.encode());
        self.wait_data_ready().inspect_err(|_| {
            let _ = self.stop();
        })?;
        self.mmio.write32(self.reg(0x0c), u32::from(value));
        self.wait_complete()?;
        Ok(())
    }
}

impl DdcBus for HardwareGmbus {
    fn read_edid_block(
        &mut self,
        address: u8,
        block_index: u8,
        block: &mut [u8; EDID_BLOCK_LEN],
    ) -> Result<(), GmaError> {
        self.acquire().inspect_err(|_error| {
            let _ = self.stop();
        })?;
        self.mmio.write32(
            self.reg(0x00),
            Gmbus0Config::conservative(self.pin).encode(),
        );
        self.mmio.write32(self.reg(0x20), 0);
        self.mmio.write32(self.reg(0x10), 0);

        // E-DDC: extension blocks live behind the segment pointer at 0x30.
        if block_index != 0 {
            self.write_byte(0x30, block_index).inspect_err(|_error| {
                let _ = self.stop();
            })?;
        }

        let command = GmbusCommand::new(
            GmbusCycle::IndexWait,
            EDID_BLOCK_LEN as u16,
            address,
            (usize::from(block_index) * EDID_BLOCK_LEN % 256) as u8,
            GmbusDirection::Read,
        )?;
        self.mmio.write32(self.reg(0x04), command.encode());

        let mut offset = 0usize;
        while offset < EDID_BLOCK_LEN {
            // Data is only valid once HARDWARE_READY is set (libgfxinit waits
            // for GMBUS2_HARDWARE_READY before each GMBUS3 read).
            self.wait_data_ready().inspect_err(|_error| {
                let _ = self.stop();
            })?;
            let word = self.mmio.read32(self.reg(0x0c)).to_le_bytes();
            let remaining = EDID_BLOCK_LEN - offset;
            let count = remaining.min(4);
            block[offset..offset + count].copy_from_slice(&word[..count]);
            offset += count;
        }
        self.wait_complete().inspect_err(|_error| {
            let _ = self.stop();
        })?;

        self.stop()
    }
}

/// Placeholder bus for explicitly unsupported DDC paths.
#[derive(Debug, Clone, Copy, Default)]
pub struct UnsupportedDdcBus;

impl DdcBus for UnsupportedDdcBus {
    fn read_edid_block(
        &mut self,
        _address: u8,
        _block_index: u8,
        _block: &mut [u8; EDID_BLOCK_LEN],
    ) -> Result<(), GmaError> {
        Err(GmaError::UnsupportedPlatform)
    }
}

/// Read, sanitize, and validate the EDID preferred mode source over DDC.
pub fn read_base_edid<'a, B: DdcBus>(
    bus: &mut B,
    storage: &'a mut [u8; EDID_BLOCK_LEN],
) -> Result<Edid<'a>, GmaError> {
    bus.read_edid_block(DDC_EDID_ADDRESS, 0, storage)?;
    *storage = edid::sanitize(*storage)?;
    Edid::parse(storage)
}

/// Read an EDID block and verify that it is compatible with the probed port.
pub fn read_compatible_base_edid<'a, B: DdcBus>(
    bus: &mut B,
    candidate: DdcProbeCandidate,
    storage: &'a mut [u8; EDID_BLOCK_LEN],
) -> Result<Edid<'a>, GmaError> {
    let edid = read_base_edid(bus, storage)?;
    if edid.compatible_with_port(candidate.port) {
        Ok(edid)
    } else {
        Err(GmaError::ModeUnavailable)
    }
}

/// Read EDID base and advertised extension blocks, then return ranked modes.
pub fn read_edid_modes<B: DdcBus>(
    bus: &mut B,
    port: Port,
    base_storage: &mut [u8; EDID_BLOCK_LEN],
    extension_storage: &mut [[u8; EDID_BLOCK_LEN]; MAX_EXTENSION_BLOCKS],
) -> Result<Vec<crate::mode::Mode, MAX_EDID_MODES>, GmaError> {
    let edid = read_base_edid(bus, base_storage)?;
    if !edid.compatible_with_port(port) {
        return Err(GmaError::ModeUnavailable);
    }
    let extension_count = usize::from(edid.extension_count()).min(MAX_EXTENSION_BLOCKS);
    let mut read_count = 0usize;
    while read_count < extension_count {
        let block_index = (read_count + 1) as u8;
        bus.read_edid_block(
            DDC_EDID_ADDRESS,
            block_index,
            &mut extension_storage[read_count],
        )?;
        read_count += 1;
    }
    Ok(edid.modes_with_extensions(&extension_storage[..read_count]))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeDdcBus {
        block: [u8; EDID_BLOCK_LEN],
    }

    impl DdcBus for FakeDdcBus {
        fn read_edid_block(
            &mut self,
            address: u8,
            block_index: u8,
            block: &mut [u8; EDID_BLOCK_LEN],
        ) -> Result<(), GmaError> {
            assert_eq!(address, DDC_EDID_ADDRESS);
            assert_eq!(block_index, 0);
            *block = self.block;
            Ok(())
        }
    }

    fn xga_edid() -> [u8; EDID_BLOCK_LEN] {
        let mut edid = [0u8; EDID_BLOCK_LEN];
        edid[..8].copy_from_slice(&[0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00]);
        edid[18] = 1;
        edid[19] = 4;
        edid[54..72].copy_from_slice(&[
            0x64, 0x19, // 65.00 MHz
            0x00, 0x40, 0x41, // hactive 1024, hblank 320
            0x00, 0x26, 0x30, // vactive 768, vblank 38
            0x18, 0x88, 0x36, 0x00, // sync offsets/widths
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ]);
        edid[127] = 0u8.wrapping_sub(edid[..127].iter().fold(0u8, |sum, b| sum.wrapping_add(*b)));
        edid
    }

    #[test]
    fn maps_current_gmbus_ports_to_ddc_pin_groups() {
        assert_eq!(ddc_pin_for_port(Port::Vga), Some(GmbusPin::Analog));
        assert_eq!(ddc_pin_for_port(Port::Lvds), Some(GmbusPin::Panel));
        assert_eq!(ddc_pin_for_port(Port::HdmiA), Some(GmbusPin::DigitalB));
        assert_eq!(ddc_pin_for_port(Port::HdmiB), Some(GmbusPin::DigitalC));
        assert_eq!(ddc_pin_for_port(Port::HdmiC), Some(GmbusPin::DigitalD));
    }

    #[test]
    fn ddc_probe_plan_models_single_port_and_legacy_order() {
        let hdmi = DdcProbePlan::for_port(Port::HdmiB).unwrap();
        assert_eq!(hdmi.len, 1);
        assert_eq!(
            hdmi.candidate(0),
            Some(DdcProbeCandidate {
                port: Port::HdmiB,
                pin: GmbusPin::DigitalC,
            })
        );
        assert_eq!(hdmi.candidate(1), None);
        assert_eq!(
            DdcProbePlan::for_port(Port::DpA),
            Err(GmaError::UnsupportedPort)
        );

        let all = DdcProbePlan::legacy_gmbus_order();
        assert_eq!(all.len, 5);
        assert_eq!(all.candidate(0).unwrap().port, Port::Vga);
        assert_eq!(all.candidate(1).unwrap().pin, GmbusPin::Panel);
        assert_eq!(all.candidate(4).unwrap().port, Port::HdmiC);
        assert_eq!(all.candidate(5), None);
    }

    #[test]
    fn gmbus_pin_select_values_match_linux_and_libgfxinit_legacy_order() {
        assert_eq!(GmbusPin::Analog.legacy_select(), 2);
        assert_eq!(GmbusPin::Panel.legacy_select(), 3);
        assert_eq!(GmbusPin::DigitalC.legacy_select(), 4);
        assert_eq!(GmbusPin::DigitalB.legacy_select(), 5);
        assert_eq!(GmbusPin::DigitalD.legacy_select(), 6);
        assert_eq!(GmbusPin::from_legacy_select(2), Some(GmbusPin::Analog));
        assert_eq!(GmbusPin::from_legacy_select(3), Some(GmbusPin::Panel));
        assert_eq!(GmbusPin::from_legacy_select(4), Some(GmbusPin::DigitalC));
        assert_eq!(GmbusPin::from_legacy_select(5), Some(GmbusPin::DigitalB));
        assert_eq!(GmbusPin::from_legacy_select(6), Some(GmbusPin::DigitalD));
        assert_eq!(GmbusPin::from_legacy_select(0), None);
        assert_eq!(GmbusPin::from_legacy_select(7), None);
    }

    #[test]
    fn gmbus0_conservative_config_encodes_pin_and_rate() {
        assert_eq!(Gmbus0Config::conservative(GmbusPin::Analog).encode(), 2);
        assert_eq!(Gmbus0Config::conservative(GmbusPin::Panel).encode(), 3);
        assert_eq!(
            Gmbus0Config {
                pin: GmbusPin::DigitalB,
                rate: GmbusRate::Khz50,
                hold_ext: true,
                byte_count_override: true,
            }
            .encode(),
            5 | (1 << 8) | (1 << 7) | (1 << 6)
        );
    }

    #[test]
    fn gmbus_command_encodes_linux_style_wait_read_index_wait_and_stop() {
        let wait = GmbusCommand::new(
            GmbusCycle::Wait,
            128,
            DDC_EDID_ADDRESS,
            0,
            GmbusDirection::Read,
        )
        .unwrap();
        assert_eq!(
            wait.encode(),
            (1 << 30) | (1 << 25) | (128 << 16) | (0x50 << 1) | 1
        );

        let indexed_wait = GmbusCommand::new(
            GmbusCycle::IndexWait,
            128,
            DDC_EDID_ADDRESS,
            0,
            GmbusDirection::Read,
        )
        .unwrap();
        assert_eq!(
            indexed_wait.encode(),
            (1 << 30) | (0b011 << 25) | (128 << 16) | (0x50 << 1) | 1
        );
        assert_eq!(GmbusCommand::stop().encode(), (1 << 30) | (4 << 25));
    }

    #[test]
    fn gmbus_command_rejects_unencodable_fields_and_exposes_checked_values() {
        let command = GmbusCommand::new(
            GmbusCycle::IndexWait,
            128,
            DDC_EDID_ADDRESS,
            0,
            GmbusDirection::Read,
        )
        .unwrap();
        assert_eq!(command.cycle(), GmbusCycle::IndexWait);
        assert_eq!(command.byte_count(), 128);
        assert_eq!(command.slave_address(), DDC_EDID_ADDRESS);
        assert_eq!(command.index(), 0);
        assert_eq!(command.direction(), GmbusDirection::Read);
        assert!(!command.timeout_enabled());
        assert!(command.software_ready());

        assert_eq!(
            GmbusCommand::new(GmbusCycle::Wait, 512, 0x50, 0, GmbusDirection::Read).err(),
            Some(GmaError::InvalidConfig)
        );
        assert_eq!(
            GmbusCommand::new(GmbusCycle::Wait, 1, 0x80, 0, GmbusDirection::Read).err(),
            Some(GmaError::InvalidConfig)
        );
    }

    #[test]
    fn gmbus_status_decodes_ready_error_and_idle_states() {
        let ready = GmbusStatus::from_bits((1 << 11) | 4);
        assert!(ready.hardware_ready);
        assert!(ready.is_data_ready());
        assert_eq!(ready.byte_count, 4);
        assert!(ready.is_wait_complete());
        assert!(!ready.has_error());

        let nak = GmbusStatus::from_bits(1 << 10);
        assert!(nak.has_error());
        assert!(nak.is_wait_complete());
        assert!(nak.is_data_ready());

        // A busy transfer (ACTIVE) is not complete yet.
        let busy = GmbusStatus::from_bits(1 << 9);
        assert!(!busy.is_wait_complete());
        assert!(!busy.is_data_ready());

        let idle = GmbusStatus::from_bits(0);
        assert!(idle.is_idle());
    }

    #[test]
    fn gmbus_edid_read_plan_matches_live_ddc_sequence() {
        let plan = GmbusReadPlan::indexed_read(GmbusPin::Analog, DDC_EDID_ADDRESS, 0, 128).unwrap();
        assert_eq!(plan.len, 6);
        assert_eq!(plan.steps[0], GmbusPlanStep::SelectPin(2));
        assert_eq!(
            plan.steps[1],
            GmbusPlanStep::Command((1 << 30) | (0b011 << 25) | (128 << 16) | (0x50 << 1) | 1)
        );
        assert_eq!(plan.steps[2], GmbusPlanStep::PollStatus);
        assert_eq!(plan.steps[3], GmbusPlanStep::ReadData { bytes: 128 });
        assert_eq!(
            plan.steps[4],
            GmbusPlanStep::Command(GmbusCommand::stop().encode())
        );
        assert_eq!(plan.steps[5], GmbusPlanStep::Disable);

        let mut bus = UnsupportedDdcBus;
        let mut storage = [0u8; EDID_BLOCK_LEN];
        assert_eq!(
            read_base_edid(&mut bus, &mut storage).err(),
            Some(GmaError::UnsupportedPlatform)
        );
    }

    #[test]
    fn dp_edp_ports_use_aux_instead_of_gmbus_pins() {
        assert_eq!(ddc_pin_for_port(Port::DpA), None);
        assert_eq!(ddc_pin_for_port(Port::DpB), None);
        assert_eq!(ddc_pin_for_port(Port::DpC), None);
        assert_eq!(ddc_pin_for_port(Port::DpD), None);
        assert_eq!(ddc_pin_for_port(Port::Edp), None);
    }

    #[test]
    fn unsupported_bus_fails_explicitly() {
        let mut bus = UnsupportedDdcBus;
        let mut storage = [0u8; EDID_BLOCK_LEN];
        assert_eq!(
            read_base_edid(&mut bus, &mut storage).err(),
            Some(GmaError::UnsupportedPlatform)
        );
    }

    #[test]
    fn read_compatible_base_edid_rejects_wrong_input_type_for_port() {
        let block = xga_edid();
        let mut bus = FakeDdcBus { block };
        let mut storage = [0u8; EDID_BLOCK_LEN];
        let candidate = DdcProbeCandidate {
            port: Port::Vga,
            pin: GmbusPin::Analog,
        };
        assert!(read_compatible_base_edid(&mut bus, candidate, &mut storage).is_ok());

        let mut digital_block = xga_edid();
        digital_block[20] = 0x80;
        digital_block[127] = 0u8.wrapping_sub(
            digital_block[..127]
                .iter()
                .fold(0u8, |sum, b| sum.wrapping_add(*b)),
        );
        let mut bus = FakeDdcBus {
            block: digital_block,
        };
        assert_eq!(
            read_compatible_base_edid(&mut bus, candidate, &mut storage).err(),
            Some(GmaError::ModeUnavailable)
        );
    }

    #[test]
    fn read_edid_modes_reads_advertised_extensions() {
        struct MultiBlockBus {
            blocks: [[u8; EDID_BLOCK_LEN]; 2],
            reads: heapless::Vec<u8, 4>,
        }

        impl DdcBus for MultiBlockBus {
            fn read_edid_block(
                &mut self,
                address: u8,
                block_index: u8,
                block: &mut [u8; EDID_BLOCK_LEN],
            ) -> Result<(), GmaError> {
                assert_eq!(address, DDC_EDID_ADDRESS);
                self.reads.push(block_index).unwrap();
                *block = self.blocks[usize::from(block_index)];
                Ok(())
            }
        }

        let mut base = xga_edid();
        base[20] |= 0x80;
        base[126] = 1;
        base[127] = 0u8.wrapping_sub(base[..127].iter().fold(0u8, |sum, b| sum.wrapping_add(*b)));
        let mut ext = [0u8; EDID_BLOCK_LEN];
        ext[0] = 0x02;
        ext[1] = 3;
        ext[2] = 4;
        ext[4..22].copy_from_slice(&[
            0x01, 0x1d, 0x00, 0x72, 0x51, 0xd0, 0x1e, 0x20, 0x6e, 0x28, 0x55, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x1e,
        ]);
        ext[127] = 0u8.wrapping_sub(ext[..127].iter().fold(0u8, |sum, b| sum.wrapping_add(*b)));

        let mut bus = MultiBlockBus {
            blocks: [base, ext],
            reads: heapless::Vec::new(),
        };
        let mut base_storage = [0u8; EDID_BLOCK_LEN];
        let mut ext_storage = [[0u8; EDID_BLOCK_LEN]; MAX_EXTENSION_BLOCKS];
        let modes =
            read_edid_modes(&mut bus, Port::HdmiA, &mut base_storage, &mut ext_storage).unwrap();
        assert_eq!(bus.reads.as_slice(), &[0, 1]);
        assert!(
            modes
                .iter()
                .any(|mode| (mode.hdisplay, mode.vdisplay) == (1280, 720))
        );
    }

    #[test]
    fn read_base_edid_sanitizes_before_parsing() {
        let mut block = xga_edid();
        block[0] = 0xaa;
        let mut bus = FakeDdcBus { block };
        let mut storage = [0u8; EDID_BLOCK_LEN];
        let edid = read_base_edid(&mut bus, &mut storage).unwrap();
        assert_eq!(edid.raw()[0], 0x00);
        assert!(edid.has_preferred_mode());
    }
}
