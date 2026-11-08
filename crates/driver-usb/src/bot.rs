// SPDX-License-Identifier: MIT OR Apache-2.0
//! Read-only bridge from CrabUSB bulk endpoints to cotton-scsi's command
//! codecs. No host controller, SCSI command-set or block driver is duplicated.
//! Transport/phase errors poison the session; no reset or command replay.
use crate::Error;
use cotton_scsi::{
    ScsiTransport,
    scsi_transport::{DataPhase, Error as ScsiError},
};
use crab_usb::{
    EndpointHandle,
    usb_if::endpoint::{TransferRequest, TransferStatus},
};

struct Buffers {
    cbw: [u8; 31],
    data: [u8; 4096],
    csw: [u8; 13],
}
pub(crate) struct Bot {
    pub input: EndpointHandle,
    pub output: EndpointHandle,
    tag: u32,
    poisoned: bool,
    buffers: &'static mut Buffers,
}
impl Bot {
    pub fn new(input: EndpointHandle, output: EndpointHandle) -> Self {
        Self {
            input,
            output,
            tag: 0,
            poisoned: false,
            buffers: alloc::boxed::Box::leak(alloc::boxed::Box::new(Buffers {
                cbw: [0; 31],
                data: [0; 4096],
                csw: [0; 13],
            })),
        }
    }
    pub fn retained_regions(&self) -> [crate::RetainedIoRegion; 3] {
        [
            crate::RetainedIoRegion {
                cpu: self.buffers.cbw.as_ptr() as usize,
                size: 31,
            },
            crate::RetainedIoRegion {
                cpu: self.buffers.data.as_ptr() as usize,
                size: 4096,
            },
            crate::RetainedIoRegion {
                cpu: self.buffers.csw.as_ptr() as usize,
                size: 13,
            },
        ]
    }
    async fn transfer(
        &mut self,
        cmd: &[u8],
        mut data: DataPhase<'_>,
    ) -> Result<usize, ScsiError<Error>> {
        self.tag = self.tag.wrapping_add(1).max(1);
        let cbw = wrapper(cmd, &data, self.tag).map_err(ScsiError::Transport)?;
        self.buffers.cbw = cbw;
        if self.tag <= 8 || self.tag.is_multiple_of(4096) {
            fstart_log::info!("usb: read-only SCSI opcode={:#x} tag={}", cmd[0], self.tag);
        }
        let out = self
            .output
            .wait(TransferRequest::bulk_out(&self.buffers.cbw))
            .await
            .map_err(|e| ScsiError::Transport(crate::transport_error(e)))?;
        if out.status != TransferStatus::Completed || out.actual_length != cbw.len() {
            return Err(ScsiError::ProtocolError);
        }
        let requested = match &data {
            DataPhase::In(buf) => buf.len(),
            _ => 0,
        };
        let actual = match &mut data {
            DataPhase::In(buf) => {
                let t = self
                    .input
                    .wait(TransferRequest::bulk_in(
                        &mut self.buffers.data[..buf.len()],
                    ))
                    .await
                    .map_err(|e| ScsiError::Transport(crate::transport_error(e)))?;
                if t.status != TransferStatus::Completed || t.actual_length > buf.len() {
                    return Err(ScsiError::ProtocolError);
                }
                buf[..t.actual_length].copy_from_slice(&self.buffers.data[..t.actual_length]);
                t.actual_length
            }
            DataPhase::None => 0,
            DataPhase::Out(_) => return Err(ScsiError::Transport(Error::Policy)),
        };
        let t = self
            .input
            .wait(TransferRequest::bulk_in(&mut self.buffers.csw))
            .await
            .map_err(|e| ScsiError::Transport(crate::transport_error(e)))?;
        if t.status != TransferStatus::Completed || t.actual_length != self.buffers.csw.len() {
            return Err(ScsiError::ProtocolError);
        }
        status(&self.buffers.csw, self.tag, requested, actual)?;
        Ok(actual)
    }
}
impl ScsiTransport for Bot {
    type Error = Error;
    async fn command(
        &mut self,
        cmd: &[u8],
        data: DataPhase<'_>,
    ) -> Result<usize, ScsiError<Error>> {
        if self.poisoned {
            return Err(ScsiError::Transport(Error::Poisoned));
        }
        let result = self.transfer(cmd, data).await;
        if matches!(
            result,
            Err(ScsiError::ProtocolError | ScsiError::Transport(_))
        ) {
            self.poisoned = true;
        }
        result
    }
}
fn wrapper(cmd: &[u8], data: &DataPhase<'_>, tag: u32) -> Result<[u8; 31], Error> {
    // Positive command/direction/length allowlist: OUT data and every writing
    // or vendor opcode are rejected BEFORE emitting even a CBW. LUN0 only.
    let len = match data {
        DataPhase::In(b) => b.len(),
        DataPhase::None => 0,
        DataPhase::Out(_) => return Err(Error::Policy),
    };
    let allowed = match cmd.first().copied() {
        Some(0x00) => cmd.len() == 6 && matches!(data, DataPhase::None),
        Some(0x03 | 0x12) => cmd.len() == 6 && matches!(data, DataPhase::In(_)),
        Some(0x25 | 0x28) => cmd.len() == 10 && matches!(data, DataPhase::In(_)),
        Some(0x88) => cmd.len() == 16 && matches!(data, DataPhase::In(_)),
        Some(0x9e) => cmd.len() == 16 && cmd[1] == 0x10 && matches!(data, DataPhase::In(_)),
        _ => false,
    };
    if !allowed || len > 4096 {
        return Err(Error::Policy);
    }
    let mut b = [0u8; 31];
    b[..4].copy_from_slice(&0x4342_5355u32.to_le_bytes());
    b[4..8].copy_from_slice(&tag.to_le_bytes());
    b[8..12].copy_from_slice(&(len as u32).to_le_bytes());
    b[12] = if matches!(data, DataPhase::In(_)) {
        0x80
    } else {
        0
    };
    b[14] = cmd.len() as u8;
    b[15..15 + cmd.len()].copy_from_slice(cmd);
    Ok(b)
}
fn status(b: &[u8; 13], tag: u32, requested: usize, actual: usize) -> Result<(), ScsiError<Error>> {
    let signature = u32::from_le_bytes(b[..4].try_into().unwrap());
    let returned_tag = u32::from_le_bytes(b[4..8].try_into().unwrap());
    let residue = u32::from_le_bytes(b[8..12].try_into().unwrap()) as usize;
    if signature != 0x5342_5355
        || returned_tag != tag
        || actual > requested
        || residue > requested
        || actual + residue != requested
    {
        return Err(ScsiError::ProtocolError);
    }
    match b[12] {
        0 => Ok(()),
        1 => Err(ScsiError::CommandFailed),
        _ => Err(ScsiError::ProtocolError),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn no_outbound_or_vendor_storage_commands_can_be_encoded() {
        for opcode in 0..=255 {
            let mut c = [0u8; 10];
            c[0] = opcode;
            assert!(wrapper(&c, &DataPhase::Out(&[1; 512]), 1).is_err());
            let mut b = [0; 512];
            if !matches!(opcode, 0x25 | 0x28) {
                assert!(wrapper(&c, &DataPhase::In(&mut b), 1).is_err());
            }
        }
        assert!(wrapper(&[], &DataPhase::None, 1).is_err());
        assert!(wrapper(&[0; 17], &DataPhase::None, 1).is_err());
    }
    #[test]
    fn cbw_is_exact_little_endian_with_big_endian_scsi_untouched() {
        let c = [0x28, 0, 0x12, 0x34, 0x56, 0x78, 0, 0, 1, 0];
        let mut data = [0; 512];
        let b = wrapper(&c, &DataPhase::In(&mut data), 0x12345678).unwrap();
        assert_eq!(
            &b[..15],
            &[
                0x55, 0x53, 0x42, 0x43, 0x78, 0x56, 0x34, 0x12, 0, 2, 0, 0, 0x80, 0, 10
            ]
        );
        assert_eq!(&b[15..25], &c);
    }
    #[test]
    fn csw_checks_signature_tag_residue_status_and_short_data() {
        let mut b = [0; 13];
        b[..4].copy_from_slice(&0x53425355u32.to_le_bytes());
        b[4..8].copy_from_slice(&7u32.to_le_bytes());
        assert!(status(&b, 7, 512, 512).is_ok());
        assert!(status(&b, 8, 512, 512).is_err());
        assert!(status(&b, 7, 512, 511).is_err());
        assert!(status(&b, 7, 512, 513).is_err());
        b[12] = 1;
        assert!(matches!(
            status(&b, 7, 512, 512),
            Err(ScsiError::CommandFailed)
        ));
        b[12] = 2;
        assert!(status(&b, 7, 512, 512).is_err());
        b[12] = 0;
        b[0] = 0;
        assert!(status(&b, 7, 512, 512).is_err());
    }
}
