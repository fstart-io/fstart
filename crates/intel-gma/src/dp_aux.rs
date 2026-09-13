//! DisplayPort AUX, DPCD, and link-training helpers.
//!
//! This module mirrors libgfxinit's DP AUX framing and provides live AUX
//! transactions for DPCD and DDC-over-AUX reads used during display probing and
//! link training.

use tock_registers::interfaces::{Readable, Writeable};

use crate::edid::EDID_BLOCK_LEN;
use crate::error::GmaError;
use crate::gmbus::DdcBus;
use crate::mmio::{Mmio, delay_us};
use crate::regs::{DP_AUX_CTL, DpAuxRegs};
use fstart_core::typed::{Mmio32, MmioAddr};

use crate::types::Port;

/// Maximum raw AUX message size handled by Intel GMA AUX data registers.
pub const AUX_MAX_MESSAGE_LEN: usize = 20;
/// Maximum accepted AUX response length (libgfxinit `Aux_Response_Length`).
pub const AUX_MAX_RESPONSE_LEN: usize = 17;
/// Maximum native AUX payload bytes after the four-byte AUX header.
pub const AUX_MAX_PAYLOAD_LEN: usize = AUX_MAX_MESSAGE_LEN - AUX_HEADER_LEN;
/// Raw AUX request header length.
pub const AUX_HEADER_LEN: usize = 4;
/// Number of Intel AUX data registers.
pub const AUX_DATA_REG_COUNT: usize = 5;
/// Native AUX write command nibble.
pub const AUX_NATIVE_WRITE: u8 = 0x8;
/// Native AUX read command nibble.
pub const AUX_NATIVE_READ: u8 = 0x9;
/// I2C-over-AUX write command nibble.
pub const AUX_I2C_WRITE: u8 = 0x0;
/// I2C-over-AUX read command nibble.
pub const AUX_I2C_READ: u8 = 0x1;
/// I2C-over-AUX middle-of-transaction bit.
pub const AUX_I2C_MOT: u8 = 0x4;
/// Standard 7-bit DDC EDID I2C address used over AUX.
pub const AUX_DDC_EDID_ADDRESS: u8 = 0x50;
/// EDID extension segment pointer I2C address used over AUX.
pub const AUX_DDC_SEGMENT_ADDRESS: u8 = 0x30;
/// AUX ACK bits in a reply header.
pub const AUX_REPLY_ACK: u8 = 0x00;
/// AUX DEFER bits in a reply header.
pub const AUX_REPLY_DEFER: u8 = 0x20;
/// AUX reply status mask.
pub const AUX_REPLY_MASK: u8 = 0x30;

/// DPCD receiver capability base address.
pub const DPCD_RECEIVER_CAPS: u32 = 0x00000;
/// DPCD link bandwidth set register.
pub const DPCD_LINK_BW_SET: u32 = 0x00100;
/// DPCD lane count set register.
pub const DPCD_LANE_COUNT_SET: u32 = 0x00101;
/// DPCD training pattern set register.
pub const DPCD_TRAINING_PATTERN_SET: u32 = 0x00102;
/// DPCD first per-lane training set register.
pub const DPCD_TRAINING_LANE0_SET: u32 = 0x00103;
/// DPCD downspread control register.
pub const DPCD_DOWNSPREAD_CTRL: u32 = 0x00107;
/// DPCD main-link channel coding register.
pub const DPCD_MAIN_LINK_CHANNEL_CODING_SET: u32 = 0x00108;
/// DPCD link status base address.
pub const DPCD_LINK_STATUS: u32 = 0x00202;

#[allow(dead_code)]
const DP_AUX_CTL_SEND_BUSY: u32 = DP_AUX_CTL::SEND_BUSY::SET.value;
#[allow(dead_code)]
const DP_AUX_CTL_DONE: u32 = DP_AUX_CTL::DONE::SET.value;
#[allow(dead_code)]
const DP_AUX_CTL_INTERRUPT_ON_DONE: u32 = DP_AUX_CTL::INTERRUPT_ON_DONE::SET.value;
#[allow(dead_code)]
const DP_AUX_CTL_TIME_OUT_ERROR: u32 = DP_AUX_CTL::TIME_OUT_ERROR::SET.value;
#[allow(dead_code)]
const DP_AUX_CTL_TIME_OUT_TIMER_MASK: u32 = DP_AUX_CTL::TIME_OUT_TIMER.val(0b11).value;
#[allow(dead_code)]
const DP_AUX_CTL_TIME_OUT_TIMER_600US: u32 = DP_AUX_CTL::TIME_OUT_TIMER::Timer600us.value;
#[allow(dead_code)]
const DP_AUX_CTL_RECEIVE_ERROR: u32 = DP_AUX_CTL::RECEIVE_ERROR::SET.value;
const DP_AUX_CTL_MESSAGE_SIZE_MASK: u32 = DP_AUX_CTL::MESSAGE_SIZE.val(0b1_1111).value;
const DP_AUX_CTL_MESSAGE_SIZE_SHIFT: u32 = 20;
#[allow(dead_code)]
const DP_AUX_CTL_PRECHARGE_TIME_MASK: u32 = DP_AUX_CTL::PRECHARGE_TIME.val(0b1111).value;
const DP_AUX_CTL_2X_BIT_CLOCK_DIV_MASK: u32 = DP_AUX_CTL::BIT_CLOCK_2X_DIVIDER.val(2047).value;
#[allow(dead_code)]
const AUX_RAW_RETRY_COUNT: usize = 3;
#[allow(dead_code)]
const AUX_DEFER_RETRY_COUNT: usize = 32;
#[allow(dead_code)]
const AUX_DEFER_DELAY_US: u32 = 500;
#[allow(dead_code)]
const AUX_BUSY_TIMEOUT: u32 = 100_000;
#[allow(dead_code)]
const AUX_DEFAULT_2X_CLOCK_DIV: u16 = 0;

/// Native AUX command kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuxNativeCommand {
    /// Native AUX write.
    Write,
    /// Native AUX read.
    Read,
}

impl AuxNativeCommand {
    const fn nibble(self) -> u8 {
        match self {
            Self::Write => AUX_NATIVE_WRITE,
            Self::Read => AUX_NATIVE_READ,
        }
    }
}

/// I2C-over-AUX command kind used for EDID/DDC probing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuxI2cCommand {
    /// I2C write.
    Write,
    /// I2C read.
    Read,
}

impl AuxI2cCommand {
    const fn nibble(self) -> u8 {
        match self {
            Self::Write => AUX_I2C_WRITE,
            Self::Read => AUX_I2C_READ,
        }
    }
}

/// Decoded raw AUX reply status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuxReplyStatus {
    /// Request was acknowledged.
    Ack,
    /// Sink deferred the request.
    Defer,
    /// Negative acknowledgement or unknown reply status.
    NakOrUnknown(u8),
}

impl AuxReplyStatus {
    /// Decode status bits from the first response byte.
    pub const fn decode(byte: u8) -> Self {
        match byte & AUX_REPLY_MASK {
            AUX_REPLY_ACK => Self::Ack,
            AUX_REPLY_DEFER => Self::Defer,
            other => Self::NakOrUnknown(other),
        }
    }
}

/// GMCH AUX register offsets for one DP port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuxRegs {
    /// AUX control register.
    pub ctl: usize,
    /// AUX data registers, in hardware order.
    pub data: [usize; AUX_DATA_REG_COUNT],
    /// Optional DDI AUX mutex register.
    pub mutex: Option<usize>,
}

impl AuxRegs {
    /// Return GMCH AUX registers for fstart's logical G45 DP ports.
    pub const fn gmch_for_port(port: Port) -> Result<Self, GmaError> {
        match port {
            Port::DpA => Ok(aux_regs(0x64110, 0x64114, Some(0x6412c))),
            Port::DpB => Ok(aux_regs(0x64210, 0x64214, Some(0x6422c))),
            Port::DpC => Ok(aux_regs(0x64310, 0x64314, Some(0x6432c))),
            _ => Err(GmaError::UnsupportedPort),
        }
    }

    /// Return split-PCH AUX registers for Ironlake/Sandy Bridge/Ivy Bridge DP ports.
    pub const fn pch_for_port(port: Port) -> Result<Self, GmaError> {
        match port {
            Port::DpB => Ok(aux_regs(0xe4110, 0xe4114, None)),
            Port::DpC => Ok(aux_regs(0xe4210, 0xe4214, None)),
            Port::DpD => Ok(aux_regs(0xe4310, 0xe4314, None)),
            _ => Err(GmaError::UnsupportedPort),
        }
    }

    /// Return DDI AUX registers for Haswell/Broadwell and later DDI platforms.
    pub const fn ddi_for_port(port: Port) -> Result<Self, GmaError> {
        match port {
            Port::Edp | Port::DpA => Ok(aux_regs(0x64010, 0x64014, Some(0x6402c))),
            Port::DpB => Ok(aux_regs(0x64110, 0x64114, Some(0x6412c))),
            Port::DpC => Ok(aux_regs(0x64210, 0x64214, Some(0x6422c))),
            Port::DpD => Ok(aux_regs(0x64310, 0x64314, Some(0x6432c))),
            _ => Err(GmaError::UnsupportedPort),
        }
    }
}

const fn aux_regs(ctl: usize, first_data: usize, mutex: Option<usize>) -> AuxRegs {
    AuxRegs {
        ctl,
        data: [
            first_data,
            first_data + 4,
            first_data + 8,
            first_data + 12,
            first_data + 16,
        ],
        mutex,
    }
}

fn aux_block(mmio: &Mmio, regs: AuxRegs) -> &'static DpAuxRegs {
    debug_assert_eq!(regs.data[0], regs.ctl + 4);
    debug_assert_eq!(regs.data[4], regs.ctl + 20);
    debug_assert!(regs.mutex.is_none_or(|mutex| mutex == regs.ctl + 0x1c));
    // SAFETY: `AuxRegs` constructors return AUX_CTL-relative register blocks
    // for decoded Intel display MMIO windows. The PCH variants do not expose a
    // mutex architecturally, but this block is only used for ctl/data there.
    unsafe { mmio.reg_block::<DpAuxRegs>(regs.ctl) }
}

/// Encode the AUX_CTL message-size field for a raw message length.
pub const fn aux_ctl_message_size(message_len: usize) -> Result<u32, GmaError> {
    if message_len == 0 || message_len > AUX_MAX_MESSAGE_LEN {
        Err(GmaError::InvalidConfig)
    } else {
        Ok((message_len as u32) << DP_AUX_CTL_MESSAGE_SIZE_SHIFT)
    }
}

/// Decode the AUX_CTL response message length field after a completed request.
pub const fn aux_ctl_response_len(ctl: u32) -> usize {
    ((ctl & DP_AUX_CTL_MESSAGE_SIZE_MASK) >> DP_AUX_CTL_MESSAGE_SIZE_SHIFT) as usize
}

/// Return the libgfxinit-style AUX_CTL value used to start a raw transaction.
pub const fn aux_ctl_start_value(message_len: usize, clock_div: u16) -> Result<u32, GmaError> {
    if (clock_div as u32) > DP_AUX_CTL_2X_BIT_CLOCK_DIV_MASK {
        return Err(GmaError::InvalidConfig);
    }
    match aux_ctl_message_size(message_len) {
        Ok(size) => Ok(DP_AUX_CTL_SEND_BUSY
            | DP_AUX_CTL_DONE
            | DP_AUX_CTL_TIME_OUT_ERROR
            | DP_AUX_CTL_RECEIVE_ERROR
            | DP_AUX_CTL_TIME_OUT_TIMER_600US
            | size
            | clock_div as u32),
        Err(err) => Err(err),
    }
}

#[allow(dead_code)]
const AUX_CTL_START_CLEAR_MASK: u32 = DP_AUX_CTL_INTERRUPT_ON_DONE
    | DP_AUX_CTL_TIME_OUT_TIMER_MASK
    | DP_AUX_CTL_MESSAGE_SIZE_MASK
    | DP_AUX_CTL_PRECHARGE_TIME_MASK
    | DP_AUX_CTL_2X_BIT_CLOCK_DIV_MASK;

/// Build a raw native AUX request into `out` and return the message length.
pub fn build_native_aux_request(
    command: AuxNativeCommand,
    address: u32,
    payload: &[u8],
    out: &mut [u8; AUX_MAX_MESSAGE_LEN],
) -> Result<usize, GmaError> {
    if address > 0x000f_ffff || payload.is_empty() || payload.len() > AUX_MAX_PAYLOAD_LEN {
        return Err(GmaError::InvalidConfig);
    }
    out.fill(0);
    out[0] = (command.nibble() << 4) | ((address >> 16) as u8 & 0x0f);
    out[1] = (address >> 8) as u8;
    out[2] = address as u8;
    out[3] = (payload.len() - 1) as u8;
    if matches!(command, AuxNativeCommand::Write) {
        out[AUX_HEADER_LEN..AUX_HEADER_LEN + payload.len()].copy_from_slice(payload);
        Ok(AUX_HEADER_LEN + payload.len())
    } else {
        Ok(AUX_HEADER_LEN)
    }
}

/// Build a native AUX read request.
pub fn build_native_read_request(
    address: u32,
    len: usize,
    out: &mut [u8; AUX_MAX_MESSAGE_LEN],
) -> Result<usize, GmaError> {
    let payload = [0u8; AUX_MAX_PAYLOAD_LEN];
    build_native_aux_request(AuxNativeCommand::Read, address, &payload[..len], out)
}

/// Build an I2C-over-AUX request into `out` and return the message length.
pub fn build_i2c_aux_request(
    command: AuxI2cCommand,
    address: u8,
    payload: &[u8],
    mot: bool,
    out: &mut [u8; AUX_MAX_MESSAGE_LEN],
) -> Result<usize, GmaError> {
    if address > 0x7f || payload.is_empty() || payload.len() > AUX_MAX_PAYLOAD_LEN {
        return Err(GmaError::InvalidConfig);
    }
    out.fill(0);
    out[0] = (command.nibble() << 4) | if_bool_u8(mot, AUX_I2C_MOT);
    out[1] = 0;
    out[2] = address;
    out[3] = (payload.len() - 1) as u8;
    if matches!(command, AuxI2cCommand::Write) {
        out[AUX_HEADER_LEN..AUX_HEADER_LEN + payload.len()].copy_from_slice(payload);
        Ok(AUX_HEADER_LEN + payload.len())
    } else {
        Ok(AUX_HEADER_LEN)
    }
}

/// Build an I2C-over-AUX EDID read request for one DDC chunk.
pub fn build_i2c_edid_read_request(
    len: usize,
    mot: bool,
    out: &mut [u8; AUX_MAX_MESSAGE_LEN],
) -> Result<usize, GmaError> {
    if len == 0 || len > AUX_MAX_PAYLOAD_LEN {
        return Err(GmaError::InvalidConfig);
    }
    let payload = [0u8; AUX_MAX_PAYLOAD_LEN];
    build_i2c_aux_request(
        AuxI2cCommand::Read,
        AUX_DDC_EDID_ADDRESS,
        &payload[..len],
        mot,
        out,
    )
}

/// Data-only I2C-over-AUX EDID request plan for one chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuxEdidReadChunkPlan {
    /// Optional segment-pointer write request length.
    pub segment_write_len: Option<usize>,
    /// Offset write request length.
    pub offset_write_len: usize,
    /// EDID data read request length.
    pub read_len: usize,
    /// Packed optional segment-pointer write request.
    pub segment_write: [u8; AUX_MAX_MESSAGE_LEN],
    /// Packed offset write request.
    pub offset_write: [u8; AUX_MAX_MESSAGE_LEN],
    /// Packed EDID data read request.
    pub read: [u8; AUX_MAX_MESSAGE_LEN],
}

impl AuxEdidReadChunkPlan {
    /// Build libgfxinit-style I2C-over-AUX DDC requests for an EDID chunk.
    pub fn new(segment: u8, offset: u8, len: usize) -> Result<Self, GmaError> {
        if len == 0 || len > AUX_MAX_PAYLOAD_LEN {
            return Err(GmaError::InvalidConfig);
        }
        let mut segment_write = [0u8; AUX_MAX_MESSAGE_LEN];
        let segment_write_len = if segment == 0 {
            None
        } else {
            Some(build_i2c_aux_request(
                AuxI2cCommand::Write,
                AUX_DDC_SEGMENT_ADDRESS,
                &[segment],
                true,
                &mut segment_write,
            )?)
        };
        let mut offset_write = [0u8; AUX_MAX_MESSAGE_LEN];
        let offset_write_len = build_i2c_aux_request(
            AuxI2cCommand::Write,
            AUX_DDC_EDID_ADDRESS,
            &[offset],
            true,
            &mut offset_write,
        )?;
        let mut read = [0u8; AUX_MAX_MESSAGE_LEN];
        let read_len = build_i2c_edid_read_request(len, false, &mut read)?;
        Ok(Self {
            segment_write_len,
            offset_write_len,
            read_len,
            segment_write,
            offset_write,
            read,
        })
    }
}

const fn if_bool_u8(condition: bool, value: u8) -> u8 {
    if condition { value } else { 0 }
}

/// Pack up to 20 AUX bytes into Intel AUX DATA register values, big-endian.
pub fn pack_aux_data(bytes: &[u8]) -> Result<[u32; AUX_DATA_REG_COUNT], GmaError> {
    if bytes.len() > AUX_MAX_MESSAGE_LEN {
        return Err(GmaError::InvalidConfig);
    }
    let mut regs = [0u32; AUX_DATA_REG_COUNT];
    for (idx, byte) in bytes.iter().enumerate() {
        let reg = idx / 4;
        let shift = 24 - ((idx % 4) * 8);
        regs[reg] |= u32::from(*byte) << shift;
    }
    Ok(regs)
}

/// Unpack Intel AUX DATA register values into bytes, big-endian.
pub fn unpack_aux_data(
    regs: [u32; AUX_DATA_REG_COUNT],
    len: usize,
    out: &mut [u8; AUX_MAX_MESSAGE_LEN],
) -> Result<(), GmaError> {
    if len > AUX_MAX_MESSAGE_LEN {
        return Err(GmaError::InvalidConfig);
    }
    out.fill(0);
    for (idx, out_byte) in out.iter_mut().take(len).enumerate() {
        let reg = idx / 4;
        let shift = 24 - ((idx % 4) * 8);
        *out_byte = (regs[reg] >> shift) as u8;
    }
    Ok(())
}

/// Perform one raw AUX transaction against explicit AUX registers.
#[allow(dead_code)]
pub(crate) fn raw_aux_request(
    mmio: &Mmio,
    regs: AuxRegs,
    request: &[u8],
    response: &mut [u8; AUX_MAX_MESSAGE_LEN],
) -> Result<usize, GmaError> {
    if request.is_empty() || request.len() > AUX_MAX_MESSAGE_LEN {
        return Err(GmaError::InvalidConfig);
    }
    let mut last_error = GmaError::HardwareError;
    for _ in 0..AUX_RAW_RETRY_COUNT {
        match raw_aux_request_once(mmio, regs, request, response) {
            Ok(len) => return Ok(len),
            Err(GmaError::Timeout) => last_error = GmaError::Timeout,
            Err(GmaError::HardwareError) => last_error = GmaError::HardwareError,
            Err(err) => return Err(err),
        }
    }
    Err(last_error)
}

#[allow(dead_code)]
fn raw_aux_request_once(
    mmio: &Mmio,
    regs: AuxRegs,
    request: &[u8],
    response: &mut [u8; AUX_MAX_MESSAGE_LEN],
) -> Result<usize, GmaError> {
    let aux = aux_block(mmio, regs);
    if aux.ctl.is_set(DP_AUX_CTL::SEND_BUSY) {
        return Err(GmaError::HardwareError);
    }
    let data = pack_aux_data(request)?;
    for (register, value) in aux.data.iter().zip(data.iter()) {
        register.set(*value);
    }
    aux.ctl.set(
        (aux.ctl.get() & !AUX_CTL_START_CLEAR_MASK)
            | aux_ctl_start_value(request.len(), AUX_DEFAULT_2X_CLOCK_DIV)?,
    );
    let mut timeout = AUX_BUSY_TIMEOUT;
    while timeout != 0 {
        if !aux.ctl.is_set(DP_AUX_CTL::SEND_BUSY) {
            break;
        }
        timeout -= 1;
        core::hint::spin_loop();
    }
    if timeout == 0 {
        return Err(GmaError::Timeout);
    }
    let ctl = aux.ctl.get();
    if (ctl & (DP_AUX_CTL_TIME_OUT_ERROR | DP_AUX_CTL_RECEIVE_ERROR)) != 0 {
        return Err(GmaError::HardwareError);
    }
    let response_len = aux_ctl_response_len(ctl);
    // libgfxinit bounds `Aux_Response_Length` to 1 .. 17; a zero-length reply
    // is malformed and longer replies are rejected here.
    if response_len == 0 || response_len > AUX_MAX_RESPONSE_LEN {
        return Err(GmaError::HardwareError);
    }
    let mut response_regs = [0u32; AUX_DATA_REG_COUNT];
    for (idx, register) in aux.data.iter().enumerate() {
        response_regs[idx] = register.get();
    }
    unpack_aux_data(response_regs, response_len, response)?;
    Ok(response_len)
}

/// Perform a native AUX read with ACK/DEFER retry handling.
#[allow(dead_code)]
pub(crate) fn native_aux_read(
    mmio: &Mmio,
    regs: AuxRegs,
    address: u32,
    out: &mut [u8],
) -> Result<(), GmaError> {
    if out.is_empty() || out.len() > AUX_MAX_PAYLOAD_LEN {
        return Err(GmaError::InvalidConfig);
    }
    let mut request = [0u8; AUX_MAX_MESSAGE_LEN];
    let request_len = build_native_read_request(address, out.len(), &mut request)?;
    let mut response = [0u8; AUX_MAX_MESSAGE_LEN];
    for _ in 0..AUX_DEFER_RETRY_COUNT {
        let response_len = raw_aux_request(mmio, regs, &request[..request_len], &mut response)?;
        if response_len == 0 {
            return Err(GmaError::HardwareError);
        }
        match AuxReplyStatus::decode(response[0]) {
            AuxReplyStatus::Ack => {
                if response_len < out.len() + 1 {
                    return Err(GmaError::HardwareError);
                }
                out.copy_from_slice(&response[1..1 + out.len()]);
                return Ok(());
            }
            AuxReplyStatus::Defer => delay_us(AUX_DEFER_DELAY_US),
            AuxReplyStatus::NakOrUnknown(_) => return Err(GmaError::HardwareError),
        }
    }
    Err(GmaError::Timeout)
}

/// Perform a native AUX write with ACK/DEFER retry handling.
#[allow(dead_code)]
pub(crate) fn native_aux_write(
    mmio: &Mmio,
    regs: AuxRegs,
    address: u32,
    payload: &[u8],
) -> Result<(), GmaError> {
    if payload.is_empty() || payload.len() > AUX_MAX_PAYLOAD_LEN {
        return Err(GmaError::InvalidConfig);
    }
    let mut request = [0u8; AUX_MAX_MESSAGE_LEN];
    let request_len =
        build_native_aux_request(AuxNativeCommand::Write, address, payload, &mut request)?;
    let mut response = [0u8; AUX_MAX_MESSAGE_LEN];
    for _ in 0..AUX_DEFER_RETRY_COUNT {
        let response_len = raw_aux_request(mmio, regs, &request[..request_len], &mut response)?;
        if response_len == 0 {
            return Err(GmaError::HardwareError);
        }
        match AuxReplyStatus::decode(response[0]) {
            AuxReplyStatus::Ack => return Ok(()),
            AuxReplyStatus::Defer => delay_us(AUX_DEFER_DELAY_US),
            AuxReplyStatus::NakOrUnknown(_) => return Err(GmaError::HardwareError),
        }
    }
    Err(GmaError::Timeout)
}

/// Perform a native AUX read on the GMCH AUX channel associated with `port`.
pub(crate) fn gmch_native_aux_read(
    mmio: &Mmio,
    port: Port,
    address: u32,
    out: &mut [u8],
) -> Result<(), GmaError> {
    native_aux_read(mmio, AuxRegs::gmch_for_port(port)?, address, out)
}

/// Perform a native AUX write on the GMCH AUX channel associated with `port`.
pub(crate) fn gmch_native_aux_write(
    mmio: &Mmio,
    port: Port,
    address: u32,
    payload: &[u8],
) -> Result<(), GmaError> {
    native_aux_write(mmio, AuxRegs::gmch_for_port(port)?, address, payload)
}

/// Hardware DDC-over-AUX reader for DisplayPort/eDP connectors.
#[derive(Debug, Clone, Copy)]
pub struct HardwareDpAuxDdc {
    mmio: Mmio,
    regs: AuxRegs,
}

impl HardwareDpAuxDdc {
    /// Create a GMCH DP AUX DDC reader for `port`.
    ///
    /// # Safety
    ///
    /// `mmio_base` must point at a decoded Intel display MMIO BAR whose AUX
    /// registers for `port` are accessible.
    pub unsafe fn gmch(mmio_base: MmioAddr<Mmio32>, port: Port) -> Result<Self, GmaError> {
        unsafe {
            Ok(Self {
                mmio: Mmio::new(mmio_base),
                regs: AuxRegs::gmch_for_port(port)?,
            })
        }
    }

    /// Create a split-PCH DP AUX DDC reader for `port`.
    ///
    /// # Safety
    ///
    /// `mmio_base` must point at a decoded Intel display MMIO BAR whose AUX
    /// registers for `port` are accessible.
    pub unsafe fn pch(mmio_base: MmioAddr<Mmio32>, port: Port) -> Result<Self, GmaError> {
        unsafe {
            Ok(Self {
                mmio: Mmio::new(mmio_base),
                regs: AuxRegs::pch_for_port(port)?,
            })
        }
    }

    /// Create a DDI AUX DDC reader for `port`.
    ///
    /// # Safety
    ///
    /// `mmio_base` must point at a decoded Intel display MMIO BAR whose AUX
    /// registers for `port` are accessible.
    pub unsafe fn ddi(mmio_base: MmioAddr<Mmio32>, port: Port) -> Result<Self, GmaError> {
        unsafe {
            Ok(Self {
                mmio: Mmio::new(mmio_base),
                regs: AuxRegs::ddi_for_port(port)?,
            })
        }
    }

    fn regs(self) -> AuxRegs {
        self.regs
    }
}

impl DdcBus for HardwareDpAuxDdc {
    fn read_edid_block(
        &mut self,
        address: u8,
        block_index: u8,
        block: &mut [u8; EDID_BLOCK_LEN],
    ) -> Result<(), GmaError> {
        if address != AUX_DDC_EDID_ADDRESS {
            return Err(GmaError::InvalidConfig);
        }
        let regs = self.regs();
        let segment = block_index / 2;
        let block_offset = usize::from(block_index % 2) * EDID_BLOCK_LEN;
        let mut offset = 0usize;
        while offset < EDID_BLOCK_LEN {
            let len = (EDID_BLOCK_LEN - offset).min(AUX_MAX_PAYLOAD_LEN);
            let plan = AuxEdidReadChunkPlan::new(segment, (block_offset + offset) as u8, len)?;
            if let Some(segment_write_len) = plan.segment_write_len {
                i2c_aux_write(&self.mmio, regs, &plan.segment_write[..segment_write_len])?;
            }
            i2c_aux_write(
                &self.mmio,
                regs,
                &plan.offset_write[..plan.offset_write_len],
            )?;
            let mut chunk = [0u8; AUX_MAX_MESSAGE_LEN];
            i2c_aux_read(
                &self.mmio,
                regs,
                &plan.read[..plan.read_len],
                &mut chunk[..len],
            )?;
            block[offset..offset + len].copy_from_slice(&chunk[..len]);
            offset += len;
        }
        Ok(())
    }
}

fn i2c_aux_write(mmio: &Mmio, regs: AuxRegs, request: &[u8]) -> Result<(), GmaError> {
    let mut response = [0u8; AUX_MAX_MESSAGE_LEN];
    for _ in 0..AUX_DEFER_RETRY_COUNT {
        let response_len = raw_aux_request(mmio, regs, request, &mut response)?;
        if response_len == 0 {
            return Err(GmaError::HardwareError);
        }
        match AuxReplyStatus::decode(response[0]) {
            AuxReplyStatus::Ack => return Ok(()),
            AuxReplyStatus::Defer => delay_us(AUX_DEFER_DELAY_US),
            AuxReplyStatus::NakOrUnknown(_) => return Err(GmaError::HardwareError),
        }
    }
    Err(GmaError::Timeout)
}

fn i2c_aux_read(
    mmio: &Mmio,
    regs: AuxRegs,
    request: &[u8],
    out: &mut [u8],
) -> Result<(), GmaError> {
    if out.is_empty() || out.len() > AUX_MAX_PAYLOAD_LEN {
        return Err(GmaError::InvalidConfig);
    }
    let mut response = [0u8; AUX_MAX_MESSAGE_LEN];
    for _ in 0..AUX_DEFER_RETRY_COUNT {
        let response_len = raw_aux_request(mmio, regs, request, &mut response)?;
        if response_len == 0 {
            return Err(GmaError::HardwareError);
        }
        match AuxReplyStatus::decode(response[0]) {
            AuxReplyStatus::Ack => {
                if response_len < out.len() + 1 {
                    return Err(GmaError::HardwareError);
                }
                out.copy_from_slice(&response[1..1 + out.len()]);
                return Ok(());
            }
            AuxReplyStatus::Defer => delay_us(AUX_DEFER_DELAY_US),
            AuxReplyStatus::NakOrUnknown(_) => return Err(GmaError::HardwareError),
        }
    }
    Err(GmaError::Timeout)
}

/// DisplayPort link bandwidth code from DPCD.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DpLinkRate {
    /// 1.62 Gbit/s per lane.
    Rbr,
    /// 2.7 Gbit/s per lane.
    Hbr,
    /// 5.4 Gbit/s per lane.
    Hbr2,
    /// Unknown but preserved DPCD code.
    Unknown(u8),
}

impl DpLinkRate {
    /// Decode a DPCD link-rate code.
    pub const fn decode(raw: u8) -> Self {
        match raw {
            0x06 => Self::Rbr,
            0x0a => Self::Hbr,
            0x14 => Self::Hbr2,
            // libgfxinit `DP_Info`: codes above 0x14 are treated as HBR2,
            // everything else unrecognised as RBR.
            other if other > 0x14 => Self::Hbr2,
            _ => Self::Rbr,
        }
    }

    /// Return the DPCD code for this rate.
    pub const fn dpcd_code(self) -> u8 {
        match self {
            Self::Rbr => 0x06,
            Self::Hbr => 0x0a,
            Self::Hbr2 => 0x14,
            Self::Unknown(raw) => raw,
        }
    }
}

/// Parsed DPCD receiver capabilities.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReceiverCaps {
    /// Raw DPCD revision byte.
    pub dpcd_revision: u8,
    /// Maximum supported link rate.
    pub max_link_rate: DpLinkRate,
    /// Maximum supported lane count: 1, 2, or 4 for normal sinks.
    pub max_lane_count: u8,
    /// Sink supports TPS3 training pattern.
    pub tps3_supported: bool,
    /// Sink supports enhanced framing.
    pub enhanced_framing: bool,
    /// Sink requires no AUX handshake during training.
    pub no_aux_handshake: bool,
    /// Raw AUX read interval field.
    pub aux_rd_interval: u8,
}

impl ReceiverCaps {
    /// Parse the 15-byte DPCD receiver caps block.
    pub fn parse(bytes: &[u8]) -> Result<Self, GmaError> {
        if bytes.len() < 15 {
            return Err(GmaError::InvalidConfig);
        }
        Ok(Self {
            dpcd_revision: bytes[0],
            max_link_rate: DpLinkRate::decode(bytes[1]),
            max_lane_count: bytes[2] & 0x1f,
            tps3_supported: (bytes[2] & 0x40) != 0,
            enhanced_framing: (bytes[2] & 0x80) != 0,
            no_aux_handshake: (bytes[3] & 0x40) != 0,
            aux_rd_interval: bytes[14] & 0x7f,
        })
    }
}

/// DP voltage-swing training level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum VoltageSwing {
    /// Level 0.
    Level0,
    /// Level 1.
    Level1,
    /// Level 2.
    Level2,
    /// Level 3.
    Level3,
}

impl VoltageSwing {
    const fn raw(self) -> u8 {
        self as u8
    }
}

/// DP pre-emphasis training level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PreEmphasis {
    /// Level 0.
    Level0,
    /// Level 1.
    Level1,
    /// Level2.
    Level2,
    /// Level 3.
    Level3,
}

impl PreEmphasis {
    const fn raw(self) -> u8 {
        self as u8
    }
}

/// One DP lane training set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrainSet {
    /// Voltage swing request.
    pub voltage_swing: VoltageSwing,
    /// Pre-emphasis request.
    pub pre_emphasis: PreEmphasis,
}

impl TrainSet {
    /// Encode a DPCD TRAINING_LANEx_SET byte.
    pub const fn encode(self, max_vs: VoltageSwing, max_pe: PreEmphasis) -> u8 {
        let vs = self.voltage_swing.raw();
        let pe = self.pre_emphasis.raw();
        let mut encoded = vs | (pe << 3);
        if vs >= max_vs.raw() {
            encoded |= 0x04;
        }
        if pe >= max_pe.raw() {
            encoded |= 0x20;
        }
        encoded
    }
}

/// Training pattern value written to DPCD TRAINING_PATTERN_SET.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrainingPattern {
    /// Disable training pattern.
    None,
    /// Training pattern 1 with scrambling disabled.
    Pattern1,
    /// Training pattern 2 with scrambling disabled.
    Pattern2,
    /// Training pattern 3 with scrambling disabled.
    Pattern3,
}

impl TrainingPattern {
    /// Encode DPCD TRAINING_PATTERN_SET like libgfxinit.
    pub const fn encode(self) -> u8 {
        match self {
            Self::None => 0x00,
            Self::Pattern1 => 0x21,
            Self::Pattern2 => 0x22,
            Self::Pattern3 => 0x23,
        }
    }
}

/// Parsed six-byte DPCD link status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinkStatus {
    bytes: [u8; 6],
}

impl LinkStatus {
    /// Parse a six-byte link status block from DPCD 0x202.
    pub fn parse(bytes: &[u8]) -> Result<Self, GmaError> {
        if bytes.len() < 6 {
            return Err(GmaError::InvalidConfig);
        }
        let mut status = [0u8; 6];
        status.copy_from_slice(&bytes[..6]);
        Ok(Self { bytes: status })
    }

    /// Return true when all active lanes have clock recovery done.
    pub const fn clock_recovery_done(self, lane_count: u8) -> bool {
        self.lanes_satisfy(lane_count, 0x01)
    }

    /// Return true when all active lanes have channel EQ, symbol lock, and align done.
    pub const fn channel_eq_done(self, lane_count: u8) -> bool {
        self.lanes_satisfy(lane_count, 0x06) && (self.bytes[2] & 0x01) != 0
    }

    const fn lanes_satisfy(self, lane_count: u8, mask: u8) -> bool {
        let mut lane = 0;
        while lane < lane_count {
            let byte = self.bytes[(lane / 2) as usize];
            let shift = (lane % 2) * 4;
            if ((byte >> shift) & mask) != mask {
                return false;
            }
            lane += 1;
        }
        true
    }

    /// Decode requested train set for a lane from ADJUST_REQUEST bytes.
    pub const fn requested_train_set(self, lane: u8) -> TrainSet {
        let byte = self.bytes[4 + (lane / 2) as usize];
        let shift = (lane % 2) * 4;
        let nibble = (byte >> shift) & 0x0f;
        TrainSet {
            voltage_swing: decode_vs(nibble & 0x03),
            pre_emphasis: decode_pe((nibble >> 2) & 0x03),
        }
    }
}

const fn decode_vs(raw: u8) -> VoltageSwing {
    match raw {
        0 => VoltageSwing::Level0,
        1 => VoltageSwing::Level1,
        2 => VoltageSwing::Level2,
        _ => VoltageSwing::Level3,
    }
}

const fn decode_pe(raw: u8) -> PreEmphasis {
    match raw {
        0 => PreEmphasis::Level0,
        1 => PreEmphasis::Level1,
        2 => PreEmphasis::Level2,
        _ => PreEmphasis::Level3,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_aux_requests_match_libgfxinit_framing() {
        let mut req = [0u8; AUX_MAX_MESSAGE_LEN];
        let len = build_native_read_request(0x202, 6, &mut req).unwrap();
        assert_eq!(len, 4);
        assert_eq!(&req[..4], &[0x90, 0x02, 0x02, 0x05]);

        let len = build_native_aux_request(
            AuxNativeCommand::Write,
            DPCD_LINK_BW_SET,
            &[0x0a, 0x84],
            &mut req,
        )
        .unwrap();
        assert_eq!(len, 6);
        assert_eq!(&req[..6], &[0x80, 0x01, 0x00, 0x01, 0x0a, 0x84]);
    }

    #[test]
    fn i2c_aux_edid_requests_match_ddc_over_aux_framing() {
        let mut req = [0u8; AUX_MAX_MESSAGE_LEN];
        let len = build_i2c_aux_request(
            AuxI2cCommand::Write,
            AUX_DDC_EDID_ADDRESS,
            &[0x80],
            true,
            &mut req,
        )
        .unwrap();
        assert_eq!(len, 5);
        assert_eq!(&req[..5], &[0x04, 0x00, 0x50, 0x00, 0x80]);

        let len = build_i2c_edid_read_request(16, false, &mut req).unwrap();
        assert_eq!(len, 4);
        assert_eq!(&req[..4], &[0x10, 0x00, 0x50, 0x0f]);
        assert_eq!(
            build_i2c_edid_read_request(AUX_MAX_PAYLOAD_LEN + 1, false, &mut req),
            Err(GmaError::InvalidConfig)
        );
    }

    #[test]
    fn aux_edid_chunk_plan_includes_segment_offset_and_read() {
        let plan = AuxEdidReadChunkPlan::new(2, 0x40, 16).unwrap();
        assert_eq!(plan.segment_write_len, Some(5));
        assert_eq!(&plan.segment_write[..5], &[0x04, 0x00, 0x30, 0x00, 0x02]);
        assert_eq!(plan.offset_write_len, 5);
        assert_eq!(&plan.offset_write[..5], &[0x04, 0x00, 0x50, 0x00, 0x40]);
        assert_eq!(plan.read_len, 4);
        assert_eq!(&plan.read[..4], &[0x10, 0x00, 0x50, 0x0f]);

        let base = AuxEdidReadChunkPlan::new(0, 0, 16).unwrap();
        assert_eq!(base.segment_write_len, None);
        assert_eq!(
            AuxEdidReadChunkPlan::new(0, 0, 0),
            Err(GmaError::InvalidConfig)
        );
    }

    #[test]
    fn aux_data_registers_pack_big_endian() {
        let regs = pack_aux_data(&[0x90, 0x02, 0x02, 0x05, 0xaa]).unwrap();
        assert_eq!(regs[0], 0x9002_0205);
        assert_eq!(regs[1], 0xaa00_0000);
        let mut bytes = [0u8; AUX_MAX_MESSAGE_LEN];
        unpack_aux_data(regs, 5, &mut bytes).unwrap();
        assert_eq!(&bytes[..5], &[0x90, 0x02, 0x02, 0x05, 0xaa]);
    }

    #[test]
    fn aux_ctl_helpers_match_libgfxinit_bit_layout() {
        assert_eq!(aux_ctl_message_size(4), Ok(4 << 20));
        assert_eq!(aux_ctl_response_len(0x0140_0000), 20);
        assert_eq!(aux_ctl_message_size(0), Err(GmaError::InvalidConfig));
        assert_eq!(aux_ctl_message_size(21), Err(GmaError::InvalidConfig));
        let start = aux_ctl_start_value(4, 0x55).unwrap();
        assert_ne!(start & DP_AUX_CTL_SEND_BUSY, 0);
        assert_ne!(start & DP_AUX_CTL_DONE, 0);
        assert_ne!(start & DP_AUX_CTL_TIME_OUT_ERROR, 0);
        assert_ne!(start & DP_AUX_CTL_RECEIVE_ERROR, 0);
        assert_eq!(
            start & DP_AUX_CTL_TIME_OUT_TIMER_MASK,
            DP_AUX_CTL_TIME_OUT_TIMER_600US
        );
        assert_eq!(start & DP_AUX_CTL_MESSAGE_SIZE_MASK, 4 << 20);
        assert_eq!(start & DP_AUX_CTL_2X_BIT_CLOCK_DIV_MASK, 0x55);
        assert_eq!(
            AUX_CTL_START_CLEAR_MASK,
            DP_AUX_CTL_INTERRUPT_ON_DONE
                | DP_AUX_CTL_TIME_OUT_TIMER_MASK
                | DP_AUX_CTL_MESSAGE_SIZE_MASK
                | DP_AUX_CTL_PRECHARGE_TIME_MASK
                | DP_AUX_CTL_2X_BIT_CLOCK_DIV_MASK
        );
    }

    #[test]
    fn aux_reply_status_decodes_ack_defer_and_nak() {
        assert_eq!(AuxReplyStatus::decode(0x00), AuxReplyStatus::Ack);
        assert_eq!(AuxReplyStatus::decode(0x20), AuxReplyStatus::Defer);
        assert_eq!(
            AuxReplyStatus::decode(0x10),
            AuxReplyStatus::NakOrUnknown(0x10)
        );
    }

    #[test]
    fn parses_receiver_caps() {
        let mut caps = [0u8; 15];
        caps[0] = 0x12;
        caps[1] = 0x0a;
        caps[2] = 0x80 | 0x40 | 4;
        caps[3] = 0x40;
        caps[14] = 7;
        let parsed = ReceiverCaps::parse(&caps).unwrap();
        assert_eq!(parsed.dpcd_revision, 0x12);
        assert_eq!(parsed.max_link_rate, DpLinkRate::Hbr);
        assert_eq!(parsed.max_lane_count, 4);
        assert!(parsed.tps3_supported);
        assert!(parsed.enhanced_framing);
        assert!(parsed.no_aux_handshake);
        assert_eq!(parsed.aux_rd_interval, 7);
    }

    #[test]
    fn train_set_and_pattern_encoding_matches_dpcd_layout() {
        let set = TrainSet {
            voltage_swing: VoltageSwing::Level2,
            pre_emphasis: PreEmphasis::Level1,
        };
        assert_eq!(set.encode(VoltageSwing::Level3, PreEmphasis::Level3), 0x0a);
        assert_eq!(set.encode(VoltageSwing::Level2, PreEmphasis::Level1), 0x2e);
        assert_eq!(TrainingPattern::None.encode(), 0x00);
        assert_eq!(TrainingPattern::Pattern1.encode(), 0x21);
        assert_eq!(TrainingPattern::Pattern2.encode(), 0x22);
        assert_eq!(TrainingPattern::Pattern3.encode(), 0x23);
    }

    #[test]
    fn parses_link_status_for_active_lanes() {
        let status = LinkStatus::parse(&[0x77, 0x77, 0x01, 0, 0x94, 0xe4]).unwrap();
        assert!(status.clock_recovery_done(1));
        assert!(status.clock_recovery_done(2));
        assert!(status.clock_recovery_done(4));
        assert!(status.channel_eq_done(1));
        assert!(status.channel_eq_done(2));
        assert!(status.channel_eq_done(4));
        assert_eq!(
            status.requested_train_set(0),
            TrainSet {
                voltage_swing: VoltageSwing::Level0,
                pre_emphasis: PreEmphasis::Level1,
            }
        );
        assert_eq!(
            status.requested_train_set(3),
            TrainSet {
                voltage_swing: VoltageSwing::Level2,
                pre_emphasis: PreEmphasis::Level3,
            }
        );

        let not_aligned = LinkStatus::parse(&[0x77, 0x77, 0x00, 0, 0, 0]).unwrap();
        assert!(not_aligned.clock_recovery_done(4));
        assert!(!not_aligned.channel_eq_done(4));
    }

    #[test]
    fn maps_gmch_aux_registers_by_logical_dp_port() {
        assert_eq!(AuxRegs::gmch_for_port(Port::DpA).unwrap().ctl, 0x64110);
        assert_eq!(AuxRegs::gmch_for_port(Port::DpB).unwrap().data[4], 0x64224);
        assert_eq!(
            AuxRegs::gmch_for_port(Port::DpC).unwrap().mutex,
            Some(0x6432c)
        );
        assert_eq!(
            AuxRegs::gmch_for_port(Port::DpD),
            Err(GmaError::UnsupportedPort)
        );
    }

    #[test]
    fn unknown_dpcd_link_rates_are_normalised_like_libgfxinit() {
        // 0x14 is HBR2; codes above it collapse to HBR2, everything else to RBR.
        assert_eq!(DpLinkRate::decode(0x14), DpLinkRate::Hbr2);
        assert_eq!(DpLinkRate::decode(0x15), DpLinkRate::Hbr2);
        assert_eq!(DpLinkRate::decode(0x20), DpLinkRate::Hbr2);
        assert_eq!(DpLinkRate::decode(0x00), DpLinkRate::Rbr);
        assert_eq!(DpLinkRate::decode(0x0f), DpLinkRate::Rbr);
        assert_eq!(DpLinkRate::decode(0x06), DpLinkRate::Rbr);
        assert_eq!(DpLinkRate::decode(0x0a), DpLinkRate::Hbr);
    }

    #[test]
    fn maps_split_pch_aux_registers_like_libgfxinit() {
        assert_eq!(AuxRegs::pch_for_port(Port::DpB).unwrap().ctl, 0xe4110);
        assert_eq!(AuxRegs::pch_for_port(Port::DpC).unwrap().data[4], 0xe4224);
        assert_eq!(AuxRegs::pch_for_port(Port::DpD).unwrap().mutex, None);
        assert_eq!(
            AuxRegs::pch_for_port(Port::DpA),
            Err(GmaError::UnsupportedPort)
        );
    }

    #[test]
    fn maps_ddi_aux_registers_like_libgfxinit() {
        assert_eq!(AuxRegs::ddi_for_port(Port::Edp).unwrap().ctl, 0x64010);
        assert_eq!(AuxRegs::ddi_for_port(Port::DpA).unwrap().data[4], 0x64024);
        assert_eq!(
            AuxRegs::ddi_for_port(Port::DpB).unwrap().mutex,
            Some(0x6412c)
        );
        assert_eq!(AuxRegs::ddi_for_port(Port::DpD).unwrap().ctl, 0x64310);
        assert_eq!(
            AuxRegs::ddi_for_port(Port::HdmiA),
            Err(GmaError::UnsupportedPort)
        );
    }
}
