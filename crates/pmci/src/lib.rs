//! DMTF PMCI transport-binding glue:
//! - PLDM over MCTP (DSP0241): [`pldm_exchange`]
//! - Secured Messages using SPDM over MCTP (DSP0276): [`SecuredMctpChannel`]
//!
//! Keeps [`fstart_mctp`], [`fstart_pldm`], and [`fstart_spdm`] strictly
//! independent peer crates so boards using e.g. SPDM over PCIe DOE do not
//! pull in MCTP.

#![no_std]

use fstart_mctp::{MctpChannel, MessageType, ReceivedMessage};
use fstart_pldm::{Error as PldmError, PldmHeader};
use fstart_spdm::{SecuredSession, SessionPhase};

/// Error returned by [`pldm_exchange`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PldmMctpError {
    Pldm(PldmError),
    Mctp(fstart_mctp::Error),
}

impl From<PldmError> for PldmMctpError {
    fn from(e: PldmError) -> Self {
        Self::Pldm(e)
    }
}

impl From<fstart_mctp::Error> for PldmMctpError {
    fn from(e: fstart_mctp::Error) -> Self {
        Self::Mctp(e)
    }
}

/// Maximum inner payload bytes that fit in a DSP0276 Secured MCTP record on
/// a default 512-byte [`fstart_mctp::Endpoint`] (`512 - 8 header - 2 app_len - 16 tag - 1 msg_type = 485`).
pub const SECURED_MCTP_MAX_PAYLOAD: usize = 485;

/// Perform a synchronous PLDM request/response exchange over any
/// [`MctpChannel`] (plain MCTP or [`SecuredMctpChannel`]).
pub fn pldm_exchange<'a, C: MctpChannel>(
    channel: &mut C,
    dst_eid: u8,
    req: &[u8],
    rx_buf: &'a mut [u8],
) -> Result<(u8, &'a [u8]), PldmMctpError> {
    let (req_hdr, _) = PldmHeader::parse(req)?;
    if !req_hdr.rq || req_hdr.datagram {
        return Err(PldmError::InvalidHeader.into());
    }
    let tag = channel.send_message(dst_eid, MessageType::Pldm as u8, None, req)?;
    let msg = channel.recv_message(rx_buf)?;
    if msg.src_eid != dst_eid
        || msg.msg_type != MessageType::Pldm as u8
        || msg.integrity_check
        || msg.tag_owner
        || msg.msg_tag != tag
    {
        return Err(PldmError::UnexpectedResponse.into());
    }
    let (resp_hdr, rest) = PldmHeader::parse(msg.payload)?;
    if resp_hdr.rq
        || resp_hdr.datagram
        || resp_hdr.instance_id != req_hdr.instance_id
        || resp_hdr.pldm_type != req_hdr.pldm_type
        || resp_hdr.command_code != req_hdr.command_code
    {
        return Err(PldmError::UnexpectedResponse.into());
    }
    let (&cc, body) = rest
        .split_first()
        .ok_or(PldmMctpError::Pldm(PldmError::ShortBuffer))?;
    Ok((cc, body))
}

/// Wraps an [`MctpChannel`] with an active [`SecuredSession`] so upper layers
/// (e.g. PLDM) run transparently over Secured MCTP (Message Type `0x06`,
/// DSP0276).
pub struct SecuredMctpChannel<'a, C> {
    pub inner: &'a mut C,
    pub session: &'a mut SecuredSession,
}

impl<C: MctpChannel> MctpChannel for SecuredMctpChannel<'_, C> {
    fn send_message(
        &mut self,
        dst_eid: u8,
        msg_type: u8,
        reply_tag: Option<u8>,
        payload: &[u8],
    ) -> Result<u8, fstart_mctp::Error> {
        if (msg_type & 0x7F) == MessageType::SecuredMsg as u8
            || self.session.phase != SessionPhase::Application
        {
            return Err(fstart_mctp::Error::InvalidHeader);
        }
        if payload.len() > SECURED_MCTP_MAX_PAYLOAD {
            return Err(fstart_mctp::Error::BufferTooSmall);
        }
        let mut app = [0u8; 1 + SECURED_MCTP_MAX_PAYLOAD];
        let mut enc = [0u8; 512];
        app[0] = msg_type & 0x7F;
        app[1..1 + payload.len()].copy_from_slice(payload);
        let elen = self
            .session
            .encrypt(&app[..1 + payload.len()], &mut enc)
            .map_err(|_| fstart_mctp::Error::Transport)?;
        self.inner.send_message(
            dst_eid,
            MessageType::SecuredMsg as u8,
            reply_tag,
            &enc[..elen],
        )
    }

    fn recv_message<'b>(
        &mut self,
        buf: &'b mut [u8],
    ) -> Result<ReceivedMessage<'b>, fstart_mctp::Error> {
        if self.session.phase != SessionPhase::Application {
            return Err(fstart_mctp::Error::InvalidHeader);
        }
        let mut enc_buf = [0u8; 576];
        let msg = self.inner.recv_message(&mut enc_buf)?;
        if msg.msg_type != MessageType::SecuredMsg as u8 {
            return Err(fstart_mctp::Error::InvalidHeader);
        }
        let mut dec_buf = [0u8; 512];
        let dlen = self
            .session
            .decrypt(msg.payload, &mut dec_buf)
            .map_err(|_| fstart_mctp::Error::BadChecksum)?;
        if dlen == 0 {
            return Err(fstart_mctp::Error::ShortBuffer);
        }
        let inner_type = dec_buf[0] & 0x7F;
        let inner_ic = (dec_buf[0] & 0x80) != 0;
        let inner_len = dlen - 1;
        if buf.len() < inner_len {
            return Err(fstart_mctp::Error::BufferTooSmall);
        }
        buf[..inner_len].copy_from_slice(&dec_buf[1..dlen]);
        Ok(ReceivedMessage {
            src_eid: msg.src_eid,
            dst_eid: msg.dst_eid,
            msg_type: inner_type,
            integrity_check: inner_ic,
            tag_owner: msg.tag_owner,
            msg_tag: msg.msg_tag,
            payload: &buf[..inner_len],
        })
    }
}
