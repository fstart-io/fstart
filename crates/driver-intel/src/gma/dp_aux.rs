//! DisplayPort AUX, DPCD, and link-training helpers.
//!
//! This module mirrors libgfxinit's DP AUX framing and provides live AUX
//! transactions for DPCD and DDC-over-AUX reads used during display probing and
//! link training.

use tock_registers::interfaces::{Readable, Writeable};

use crate::gma::edid::EDID_BLOCK_LEN;
use crate::gma::error::GmaError;
use crate::gma::gmbus::DdcBus;
use crate::gma::mmio::{Mmio, delay_us};
use crate::gma::regs::{DP_AUX_CTL, DpAuxRegs};
use fstart_core::typed::{Mmio32, MmioAddr};

use crate::gma::types::Port;

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
const DP_AUX_CTL_2X_BIT_CLOCK_DIV_MASK: u32 = DP_AUX_CTL::BIT_CLOCK_2X_DIVIDER.val(2047).value;
#[allow(dead_code)]
const AUX_RAW_RETRY_COUNT: usize = 3;
#[allow(dead_code)]
const AUX_DEFER_RETRY_COUNT: usize = 32;
#[allow(dead_code)]
const AUX_DEFER_DELAY_US: u32 = 500;
/// libgfxinit `Registers.Default_Timeout_MS`.
const AUX_BUSY_TIMEOUT_US: u64 = 10_000;
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

/// How a transaction programs the AUX_CTL 2x bit-clock divider
/// (libgfxinit `DP_AUX_CTL_2x_Clock` and its mask).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuxClock {
    /// `Has_PCH_Aux_Channels`: the divider field is cleared and rewritten.
    Replace(u16),
    /// `Has_GMCH_RawClk`: the divider is OR'd into the field, which is not
    /// cleared first.
    Merge(u16),
}

impl AuxClock {
    /// libgfxinit's PCH divider, `(clock + 1 MHz) / 2 MHz`.
    pub const fn pch(clock_hz: u64) -> Self {
        Self::Replace(((clock_hz + 1_000_000) / 2_000_000) as u16)
    }

    /// libgfxinit's GMCH divider, `Div_Round_Closest (Raw_Clock, 2 MHz)`.
    pub const fn gmch(raw_clock_hz: u64) -> Self {
        Self::Merge(((raw_clock_hz + 1_000_000) / 2_000_000) as u16)
    }

    const fn mask(self) -> u32 {
        match self {
            Self::Replace(_) => DP_AUX_CTL_2X_BIT_CLOCK_DIV_MASK,
            Self::Merge(_) => 0,
        }
    }

    const fn divider(self) -> u16 {
        match self {
            Self::Replace(div) | Self::Merge(div) => div,
        }
    }
}

/// AUX channel register offsets for one DP port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuxRegs {
    /// AUX control register.
    pub ctl: usize,
    /// AUX data registers, in hardware order.
    pub data: [usize; AUX_DATA_REG_COUNT],
    /// Optional DDI AUX mutex register.
    pub mutex: Option<usize>,
    /// 2x bit-clock divider programming.
    pub clock: AuxClock,
}

impl AuxRegs {
    /// Return GMCH AUX registers for fstart's logical G45 DP ports, whose
    /// divider derives from the GMCH raw (FSB) clock.
    pub const fn gmch_for_port(port: Port, raw_clock_hz: u64) -> Result<Self, GmaError> {
        let clock = AuxClock::gmch(raw_clock_hz);
        match port {
            Port::DpA => Ok(aux_regs(0x64110, 0x64114, Some(0x6412c), clock)),
            Port::DpB => Ok(aux_regs(0x64210, 0x64214, Some(0x6422c), clock)),
            Port::DpC => Ok(aux_regs(0x64310, 0x64314, Some(0x6432c), clock)),
            _ => Err(GmaError::UnsupportedPort),
        }
    }

    /// Return split-PCH AUX registers for Ironlake/Sandy Bridge/Ivy Bridge.
    ///
    /// eDP uses the CPU channel `DP_AUX_CTL_A`, clocked from CDClk; DP1..3
    /// (`DpA`..`DpC`, PCH ports B..D) use the PCH channels clocked from the
    /// raw clock (libgfxinit `DP_Aux_Request`).
    pub const fn pch_for_port(
        port: Port,
        cdclk_hz: u64,
        raw_clock_hz: u64,
    ) -> Result<Self, GmaError> {
        let pch = AuxClock::pch(raw_clock_hz);
        match port {
            Port::Edp => Ok(aux_regs(0x64010, 0x64014, None, AuxClock::pch(cdclk_hz))),
            Port::DpA => Ok(aux_regs(0xe4110, 0xe4114, None, pch)),
            Port::DpB => Ok(aux_regs(0xe4210, 0xe4214, None, pch)),
            Port::DpC => Ok(aux_regs(0xe4310, 0xe4314, None, pch)),
            _ => Err(GmaError::UnsupportedPort),
        }
    }

    /// Return DDI AUX registers for Haswell/Broadwell and later DDI platforms.
    pub const fn ddi_for_port(port: Port) -> Result<Self, GmaError> {
        // DDI generations have no executor yet; their divider is left zero.
        let clock = AuxClock::Replace(AUX_DEFAULT_2X_CLOCK_DIV);
        match port {
            Port::Edp | Port::DpA => Ok(aux_regs(0x64010, 0x64014, Some(0x6402c), clock)),
            Port::DpB => Ok(aux_regs(0x64110, 0x64114, Some(0x6412c), clock)),
            Port::DpC => Ok(aux_regs(0x64210, 0x64214, Some(0x6422c), clock)),
            Port::DpD => Ok(aux_regs(0x64310, 0x64314, Some(0x6432c), clock)),
            _ => Err(GmaError::UnsupportedPort),
        }
    }
}

const fn aux_regs(ctl: usize, first_data: usize, mutex: Option<usize>, clock: AuxClock) -> AuxRegs {
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
        clock,
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

/// Fields libgfxinit clears before starting a transfer; the divider field
/// is added by [`AuxClock::mask`] for PCH-style channels only.
const AUX_CTL_START_CLEAR_MASK: u32 =
    DP_AUX_CTL_INTERRUPT_ON_DONE | DP_AUX_CTL_TIME_OUT_TIMER_MASK | DP_AUX_CTL_MESSAGE_SIZE_MASK;

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
    // `Write_Data_Reg` only touches registers that carry request bytes.
    for (register, value) in aux
        .data
        .iter()
        .zip(data.iter())
        .take(request.len().div_ceil(4))
    {
        register.set(*value);
    }
    aux.ctl.set(
        (aux.ctl.get() & !(AUX_CTL_START_CLEAR_MASK | regs.clock.mask()))
            | aux_ctl_start_value(request.len(), regs.clock.divider())?,
    );
    // libgfxinit ignores the busy wait's timeout and judges the transfer by
    // the error bits alone.
    let _ = mmio.wait_mask(regs.ctl, DP_AUX_CTL_SEND_BUSY, 0, AUX_BUSY_TIMEOUT_US);
    let ctl = aux.ctl.get();
    if (ctl & (DP_AUX_CTL_TIME_OUT_ERROR | DP_AUX_CTL_RECEIVE_ERROR)) != 0 {
        return Err(GmaError::HardwareError);
    }
    // libgfxinit `Aux_Response_Length` is 1 .. 17: an empty reply fails,
    // a longer one is clamped.
    let response_len = aux_ctl_response_len(ctl).min(AUX_MAX_RESPONSE_LEN);
    if response_len == 0 {
        return Err(GmaError::HardwareError);
    }
    let mut response_regs = [0u32; AUX_DATA_REG_COUNT];
    for (idx, register) in aux.data.iter().enumerate().take(response_len.div_ceil(4)) {
        response_regs[idx] = register.get();
    }
    unpack_aux_data(response_regs, response_len, response)?;
    Ok(response_len)
}

/// libgfxinit `DP_Aux_Ch.Do_Aux_Request`: repeat the raw request while the
/// sink defers it natively, then require a native ACK. I2C-level replies are
/// left in `response` for the caller.
fn aux_transaction(
    mmio: &Mmio,
    regs: AuxRegs,
    request: &[u8],
    response: &mut [u8; AUX_MAX_MESSAGE_LEN],
) -> Result<usize, GmaError> {
    let mut len = 0;
    for _ in 0..AUX_DEFER_RETRY_COUNT {
        len = raw_aux_request(mmio, regs, request, response)?;
        if AuxReplyStatus::decode(response[0]) != AuxReplyStatus::Defer {
            break;
        }
        delay_us(AUX_DEFER_DELAY_US);
    }
    match AuxReplyStatus::decode(response[0]) {
        AuxReplyStatus::Ack => Ok(len),
        AuxReplyStatus::Defer => Err(GmaError::Timeout),
        AuxReplyStatus::NakOrUnknown(_) => Err(GmaError::HardwareError),
    }
}

/// Perform a native AUX read (libgfxinit `Aux_Read`).
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
    let response_len = aux_transaction(mmio, regs, &request[..request_len], &mut response)?;
    // Callers need the whole block (libgfxinit compares the returned length).
    if response_len < out.len() + 1 {
        return Err(GmaError::HardwareError);
    }
    out.copy_from_slice(&response[1..1 + out.len()]);
    Ok(())
}

/// Perform a native AUX write (libgfxinit `Aux_Write`).
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
    aux_transaction(mmio, regs, &request[..request_len], &mut response).map(|_| ())
}

/// GMCH AUX registers for `port`, clocked from the FSB raw clock.
fn gmch_aux_regs(mmio: &Mmio, port: Port) -> Result<AuxRegs, GmaError> {
    AuxRegs::gmch_for_port(port, crate::gma::power::gmch_raw_clock_hz(mmio))
}

const AUX_I2C_WR_STATUS_REQ: u8 = 0x2;
/// I2C DEFER replies `I2C_Out_Packet`/`I2C_In_Packet` tolerate.
const I2C_MAX_DEFERS: usize = 7;
/// Consecutive short replies `Do_I2C_Read` tolerates.
const I2C_MAX_READ_TRIES: usize = 4;

/// I2C-level reply status in bits 7:6 of the first reply byte.
const fn i2c_ack(reply: u8) -> bool {
    reply & 0xc0 == 0x00
}

const fn i2c_defer(reply: u8) -> bool {
    reply & 0xc0 == 0x80
}

/// libgfxinit `Fill_Aux_Request` for I2C-over-AUX. A zero length yields the
/// three-byte address-only request (`Is_Empty`).
fn i2c_request(
    command: u8,
    address: u8,
    payload: &[u8],
    read_len: usize,
    out: &mut [u8; AUX_MAX_MESSAGE_LEN],
) -> usize {
    out.fill(0);
    out[0] = command << 4;
    out[2] = address;
    let len = payload.len().max(read_len);
    if len == 0 {
        out[3] = 0xff;
        return 3;
    }
    out[3] = (len - 1) as u8;
    out[AUX_HEADER_LEN..AUX_HEADER_LEN + payload.len()].copy_from_slice(payload);
    AUX_HEADER_LEN + payload.len()
}

/// libgfxinit `I2C_Out_Packet`: write `data`, polling the write status while
/// the sink defers or accepts only part of it.
fn i2c_out_packet(
    mmio: &Mmio,
    regs: AuxRegs,
    command: u8,
    address: u8,
    data: &[u8],
) -> Result<(), GmaError> {
    let mut request = [0u8; AUX_MAX_MESSAGE_LEN];
    let mut request_len = i2c_request(command, address, data, 0, &mut request);
    let mut response = [0u8; AUX_MAX_MESSAGE_LEN];
    let mut transferred = 0usize;
    let mut tries = 0usize;
    loop {
        let response_len = aux_transaction(mmio, regs, &request[..request_len], &mut response)?;
        if i2c_ack(response[0]) {
            if response_len <= 1 {
                transferred = data.len();
            } else {
                transferred += usize::from(response[1]).min(data.len() - transferred);
                tries = 0;
            }
            if transferred == data.len() {
                return Ok(());
            }
        } else if i2c_defer(response[0]) {
            if tries >= I2C_MAX_DEFERS {
                return Err(GmaError::Timeout);
            }
            tries += 1;
        } else {
            return Err(GmaError::HardwareError);
        }
        request_len = i2c_request(
            (command & AUX_I2C_MOT) | AUX_I2C_WR_STATUS_REQ,
            address,
            &[],
            0,
            &mut request,
        );
        delay_us(AUX_DEFER_DELAY_US);
    }
}

/// libgfxinit `I2C_In_Packet`: request `len` bytes, retrying I2C DEFERs.
fn i2c_in_packet(
    mmio: &Mmio,
    regs: AuxRegs,
    command: u8,
    address: u8,
    len: usize,
    response: &mut [u8; AUX_MAX_MESSAGE_LEN],
) -> Result<usize, GmaError> {
    let mut request = [0u8; AUX_MAX_MESSAGE_LEN];
    let request_len = i2c_request(command, address, &[], len, &mut request);
    let mut response_len = 0;
    for _ in 0..I2C_MAX_DEFERS {
        response_len = aux_transaction(mmio, regs, &request[..request_len], response)?;
        if !i2c_defer(response[0]) {
            break;
        }
        delay_us(AUX_DEFER_DELAY_US);
    }
    if i2c_ack(response[0]) {
        Ok(response_len)
    } else {
        Err(GmaError::HardwareError)
    }
}

/// libgfxinit `I2C_Empty_Packet`: an address-only transaction.
fn i2c_empty_packet(mmio: &Mmio, regs: AuxRegs, command: u8, address: u8) -> Result<(), GmaError> {
    let mut response = [0u8; AUX_MAX_MESSAGE_LEN];
    i2c_in_packet(mmio, regs, command, address, 0, &mut response).map(|_| ())
}

/// libgfxinit `Do_I2C_Write`. With `stop == false` the closing address-only
/// write is left out, so a following transfer continues the transaction
/// (needed after an E-DDC segment pointer write).
fn do_i2c_write(
    mmio: &Mmio,
    regs: AuxRegs,
    address: u8,
    data: &[u8],
    stop: bool,
) -> Result<(), GmaError> {
    i2c_empty_packet(mmio, regs, AUX_I2C_WRITE | AUX_I2C_MOT, address)?;
    let result = i2c_out_packet(mmio, regs, AUX_I2C_WRITE | AUX_I2C_MOT, address, data);
    if stop {
        // The transfer's own status decides; the stop is best effort.
        let _ = i2c_empty_packet(mmio, regs, AUX_I2C_WRITE, address);
    }
    result
}

/// libgfxinit `Do_I2C_Read`: read `out.len()` bytes in chunks of at most 16.
fn do_i2c_read(mmio: &Mmio, regs: AuxRegs, address: u8, out: &mut [u8]) -> Result<(), GmaError> {
    i2c_empty_packet(mmio, regs, AUX_I2C_READ | AUX_I2C_MOT, address)?;
    let mut transferred = 0usize;
    let mut chunk = AUX_MAX_PAYLOAD_LEN;
    let mut tries = 0usize;
    let mut response = [0u8; AUX_MAX_MESSAGE_LEN];
    while transferred < out.len() && tries < I2C_MAX_READ_TRIES {
        let response_len = i2c_in_packet(
            mmio,
            regs,
            AUX_I2C_READ | AUX_I2C_MOT,
            address,
            chunk.min(out.len() - transferred),
            &mut response,
        )?;
        if response_len >= 2 {
            chunk = (response_len - 1).min(out.len() - transferred);
            out[transferred..transferred + chunk].copy_from_slice(&response[1..1 + chunk]);
            transferred += chunk;
            tries = 0;
        } else {
            tries += 1;
        }
    }
    let _ = i2c_empty_packet(mmio, regs, AUX_I2C_READ, address);
    if tries < I2C_MAX_READ_TRIES {
        Ok(())
    } else {
        Err(GmaError::HardwareError)
    }
}

/// libgfxinit `I2C_Read_Byte`: write the register offset, read one byte.
pub(crate) fn i2c_read_byte(
    mmio: &Mmio,
    regs: AuxRegs,
    address: u8,
    offset: u8,
) -> Result<u8, GmaError> {
    do_i2c_write(mmio, regs, address, &[offset], true)?;
    let mut value = [0u8];
    do_i2c_read(mmio, regs, address, &mut value)?;
    Ok(value[0])
}

/// libgfxinit `I2C_Write_Byte`.
pub(crate) fn i2c_write_byte(
    mmio: &Mmio,
    regs: AuxRegs,
    address: u8,
    offset: u8,
    value: u8,
) -> Result<(), GmaError> {
    do_i2c_write(mmio, regs, address, &[offset, value], true)
}

/// libgfxinit `I2C_Read`: write index 0, then read `out.len()` bytes.
pub(crate) fn i2c_read(
    mmio: &Mmio,
    regs: AuxRegs,
    address: u8,
    out: &mut [u8],
) -> Result<(), GmaError> {
    do_i2c_write(mmio, regs, address, &[0], true)?;
    do_i2c_read(mmio, regs, address, out)
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
        // SAFETY: forwarded caller contract.
        let mmio = unsafe { Mmio::new(mmio_base) };
        Ok(Self {
            mmio,
            regs: gmch_aux_regs(&mmio, port)?,
        })
    }

    /// Create a split-PCH DP AUX DDC reader for `port` on `cpu`.
    ///
    /// # Safety
    ///
    /// `mmio_base` must point at a decoded Intel display MMIO BAR whose AUX
    /// registers for `port` are accessible.
    pub unsafe fn pch(
        mmio_base: MmioAddr<Mmio32>,
        cpu: crate::gma::types::Cpu,
        port: Port,
    ) -> Result<Self, GmaError> {
        // SAFETY: forwarded caller contract.
        let mmio = unsafe { Mmio::new(mmio_base) };
        Ok(Self {
            mmio,
            regs: crate::gma::generation::ironlake::aux_regs(cpu, port)?,
        })
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
        let segment = block_index / 2;
        let offset = (block_index % 2) * (EDID_BLOCK_LEN as u8);
        if segment == 0 && offset == 0 {
            // libgfxinit `Display_Probing.Read_EDID`.
            return i2c_read(&self.mmio, self.regs, address, block);
        }
        // E-DDC: segment pointer and offset without a stop in between.
        if segment != 0 {
            do_i2c_write(
                &self.mmio,
                self.regs,
                AUX_DDC_SEGMENT_ADDRESS,
                &[segment],
                false,
            )?;
        }
        do_i2c_write(&self.mmio, self.regs, address, &[offset], segment == 0)?;
        do_i2c_read(&self.mmio, self.regs, address, block)
    }
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
            // libgfxinit `Read_Caps`: 0|1 -> 1, 2|3 -> 2, others -> 4.
            max_lane_count: match bytes[2] & 0x1f {
                0 | 1 => 1,
                2 | 3 => 2,
                _ => 4,
            },
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

    /// Return true when all active lanes have clock recovery, channel EQ and
    /// symbol lock, and interlane alignment is done (libgfxinit `All_EQ_Done`).
    pub const fn channel_eq_done(self, lane_count: u8) -> bool {
        self.lanes_satisfy(lane_count, 0x07) && (self.bytes[2] & 0x01) != 0
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
    fn i2c_requests_match_libgfxinit_fill_aux_request() {
        let mut req = [0u8; AUX_MAX_MESSAGE_LEN];
        // Address-only start of a MOT write: three bytes, length 0xff.
        assert_eq!(
            i2c_request(AUX_I2C_WRITE | AUX_I2C_MOT, 0x50, &[], 0, &mut req),
            3
        );
        assert_eq!(&req[..4], &[0x40, 0x00, 0x50, 0xff]);
        assert_eq!(
            i2c_request(AUX_I2C_WRITE | AUX_I2C_MOT, 0x50, &[0x80], 0, &mut req),
            5
        );
        assert_eq!(&req[..5], &[0x40, 0x00, 0x50, 0x00, 0x80]);
        assert_eq!(
            i2c_request(AUX_I2C_READ | AUX_I2C_MOT, 0x50, &[], 16, &mut req),
            4
        );
        assert_eq!(&req[..4], &[0x50, 0x00, 0x50, 0x0f]);
        assert!(i2c_ack(0x00) && !i2c_ack(0x40) && i2c_defer(0x80));
    }

    #[test]
    fn aux_clock_dividers_match_libgfxinit() {
        // Ironlake/SNB: CDClk for the CPU channel, 125 MHz raw clock for PCH.
        assert_eq!(AuxClock::pch(450_000_000), AuxClock::Replace(225));
        assert_eq!(AuxClock::pch(400_000_000), AuxClock::Replace(200));
        assert_eq!(AuxClock::pch(125_000_000), AuxClock::Replace(63));
        // GMCH: rounded to the closest divider and OR'd in.
        assert_eq!(AuxClock::gmch(133_333_333), AuxClock::Merge(67));
        let edp = AuxRegs::pch_for_port(Port::Edp, 400_000_000, 125_000_000).unwrap();
        assert_eq!((edp.ctl, edp.clock), (0x64010, AuxClock::Replace(200)));
        let dp2 = AuxRegs::pch_for_port(Port::DpB, 400_000_000, 125_000_000).unwrap();
        assert_eq!((dp2.ctl, dp2.data[0]), (0xe4210, 0xe4214));
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
    fn aux_ctl_rejects_invalid_message_sizes() {
        assert_eq!(aux_ctl_message_size(0), Err(GmaError::InvalidConfig));
        assert_eq!(aux_ctl_message_size(21), Err(GmaError::InvalidConfig));
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
    fn train_set_marks_the_maximum_drive_levels() {
        let set = TrainSet {
            voltage_swing: VoltageSwing::Level2,
            pre_emphasis: PreEmphasis::Level1,
        };
        assert_eq!(set.encode(VoltageSwing::Level3, PreEmphasis::Level3), 0x0a);
        assert_eq!(set.encode(VoltageSwing::Level2, PreEmphasis::Level1), 0x2e);
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
    fn gmch_aux_rejects_unsupported_ports() {
        assert_eq!(
            AuxRegs::gmch_for_port(Port::DpD, 133_333_333),
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
    fn pch_aux_rejects_unsupported_ports() {
        assert_eq!(
            AuxRegs::pch_for_port(Port::DpD, 400_000_000, 125_000_000),
            Err(GmaError::UnsupportedPort)
        );
    }

    #[test]
    fn ddi_aux_rejects_unsupported_ports() {
        assert_eq!(
            AuxRegs::ddi_for_port(Port::HdmiA),
            Err(GmaError::UnsupportedPort)
        );
    }
}
