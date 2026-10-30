// SPDX-License-Identifier: MIT OR Apache-2.0
//! The TPM FIFO interface: TIS 1.3 (TPM 1.2) and its PTP successor for
//! TPM 2.0, memory mapped one 4 KiB page per locality.
//!
//! Registers are accessed bytewise where the TIS defines write-one side
//! effects (status) and where LPC TPMs only decode byte cycles (FIFO, burst
//! count). The command protocol follows TIS 1.3 §5.5 and PTP 1.07 §6.5.

use embedded_hal::delay::DelayNs;
use fstart_core::mmio::{MmioReadOnly, MmioReadWrite};
use tock_registers::interfaces::{Readable, Writeable};
use tock_registers::{LocalRegisterCopy, register_bitfields, register_structs};

use crate::{Error, Family, HEADER_SIZE, Transport};

/// Locality 0 on PC platforms (PTP §6.3.2).
pub const BASE: usize = 0xFED4_0000;
/// One page per locality, five localities.
pub const LOCALITY_SIZE: usize = 0x1000;
pub const SIZE: usize = 5 * LOCALITY_SIZE;

register_bitfields![u8,
    /// `TPM_ACCESS_x`.
    pub Access [
        ESTABLISHMENT OFFSET(0) NUMBITS(1) [],
        REQUEST_USE OFFSET(1) NUMBITS(1) [],
        PENDING_REQUEST OFFSET(2) NUMBITS(1) [],
        SEIZE OFFSET(3) NUMBITS(1) [],
        BEEN_SEIZED OFFSET(4) NUMBITS(1) [],
        ACTIVE_LOCALITY OFFSET(5) NUMBITS(1) [],
        VALID OFFSET(7) NUMBITS(1) []
    ],
    /// `TPM_STS_x` bits 7:0.
    pub Sts [
        RESPONSE_RETRY OFFSET(1) NUMBITS(1) [],
        SELF_TEST_DONE OFFSET(2) NUMBITS(1) [],
        EXPECT OFFSET(3) NUMBITS(1) [],
        DATA_AVAIL OFFSET(4) NUMBITS(1) [],
        GO OFFSET(5) NUMBITS(1) [],
        COMMAND_READY OFFSET(6) NUMBITS(1) [],
        VALID OFFSET(7) NUMBITS(1) []
    ],
    /// `TPM_STS_x` bits 31:24 (PTP only; reserved on TIS 1.3).
    pub Sts3 [
        COMMAND_CANCEL OFFSET(0) NUMBITS(1) [],
        RESET_ESTABLISHMENT OFFSET(1) NUMBITS(1) [],
        FAMILY OFFSET(2) NUMBITS(2) [Tpm12 = 0, Tpm20 = 1]
    ]
];

register_bitfields![u32,
    /// `TPM_INTF_CAPABILITY_x`.
    pub IntfCapability [
        DATA_TRANSFER_SIZE OFFSET(9) NUMBITS(2) [],
        BURST_COUNT_STATIC OFFSET(8) NUMBITS(1) [],
        INTERFACE_VERSION OFFSET(28) NUMBITS(3) [Tis12 = 0, Tis13 = 2, PtpFifo = 3]
    ],
    /// `TPM_INTERFACE_ID_x` (PTP; all ones on TIS 1.3 parts).
    pub InterfaceId [
        INTERFACE_TYPE OFFSET(0) NUMBITS(4) [Fifo = 0, Crb = 1, Tis13 = 0xF],
        INTERFACE_VERSION OFFSET(4) NUMBITS(4) [],
        CAP_LOCALITY OFFSET(8) NUMBITS(1) [],
        CAP_FIFO OFFSET(13) NUMBITS(1) [],
        CAP_CRB OFFSET(14) NUMBITS(1) [],
        INTERFACE_SELECTOR OFFSET(17) NUMBITS(2) [Fifo = 0, Crb = 1],
        INTF_SEL_LOCK OFFSET(19) NUMBITS(1) [],
        RID OFFSET(24) NUMBITS(8) []
    ],
    /// `TPM_DID_VID_x`.
    pub DidVid [
        VID OFFSET(0) NUMBITS(16) [],
        DID OFFSET(16) NUMBITS(16) []
    ]
];

register_structs! {
    /// One locality's register page.
    pub Locality {
        (0x000 => pub access: MmioReadWrite<u8, Access::Register>),
        (0x001 => _reserved0),
        (0x008 => pub int_enable: MmioReadWrite<u32>),
        (0x00C => pub int_vector: MmioReadWrite<u8>),
        (0x00D => _reserved1),
        (0x010 => pub int_status: MmioReadWrite<u32>),
        (0x014 => pub intf_capability: MmioReadOnly<u32, IntfCapability::Register>),
        (0x018 => pub sts: MmioReadWrite<u8, Sts::Register>),
        (0x019 => pub burst_count: [MmioReadOnly<u8>; 2]),
        (0x01B => pub sts3: MmioReadWrite<u8, Sts3::Register>),
        (0x01C => _reserved2),
        (0x024 => pub data_fifo: MmioReadWrite<u8>),
        (0x025 => _reserved3),
        (0x030 => pub interface_id: MmioReadWrite<u32, InterfaceId::Register>),
        (0x034 => _reserved4),
        (0xF00 => pub did_vid: MmioReadOnly<u32, DidVid::Register>),
        (0xF04 => pub rid: MmioReadOnly<u8>),
        (0xF05 => _reserved5),
        (0x1000 => @END),
    }
}

/// TIS timeouts A to D (TIS 1.3 table 13), in microseconds. C and D are the
/// TPM 1.2 values; PTP allows TPM 2.0 shorter ones (200 and 30 ms), so these
/// only wait longer.
const TIMEOUT_A: u32 = 750_000;
const TIMEOUT_B: u32 = 2_000_000;
const TIMEOUT_C: u32 = 750_000;
const TIMEOUT_D: u32 = 750_000;
/// The longest command firmware sends (self tests, 1.2 SelfTestFull).
const TIMEOUT_COMMAND: u32 = 120_000_000;
const POLL_US: u32 = 10;
/// Re-reads of one response before giving up.
const RESPONSE_RETRIES: u32 = 3;

/// What the interface reports before any command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Identity {
    /// Family by the interface version; `TPM2_Startup` has the last word.
    pub family: Family,
    pub vendor: u16,
    pub device: u16,
    pub revision: u8,
}

/// A FIFO TPM at a locality.
pub struct Tis<D> {
    base: usize,
    locality: u8,
    delay: D,
}

impl<D: DelayNs> Tis<D> {
    /// The interface with locality 0 at `base`.
    ///
    /// # Safety
    /// `base..base + SIZE` decodes to the TPM (or to nothing) and no other
    /// code drives it while this value exists.
    pub const unsafe fn new(base: usize, delay: D) -> Self {
        Self {
            base,
            locality: 0,
            delay,
        }
    }

    /// Select the locality used by later commands; claimed on use.
    pub fn set_locality(&mut self, locality: u8) {
        self.locality = locality.min(4);
    }

    fn regs(&self, locality: u8) -> &Locality {
        // SAFETY: `new` guarantees the window; the page is in range.
        unsafe { &*((self.base + usize::from(locality) * LOCALITY_SIZE) as *const Locality) }
    }

    fn wait(&mut self, timeout_us: u32, mut done: impl FnMut(&Locality) -> bool) -> bool {
        let mut waited = 0;
        loop {
            if done(self.regs(self.locality)) {
                return true;
            }
            if waited >= timeout_us {
                return false;
            }
            self.delay.delay_us(POLL_US);
            waited += POLL_US;
        }
    }

    /// Identify the TPM, or `None` when nothing answers at the base.
    pub fn probe(&self) -> Option<Identity> {
        let regs = self.regs(0);
        let access = regs.access.get();
        if access == 0xFF || !regs.access.is_set(Access::VALID) {
            return None;
        }
        let id = regs.interface_id.extract();
        let ptp = id.get() != u32::MAX
            && id.matches_any(&[
                InterfaceId::INTERFACE_TYPE::Fifo,
                InterfaceId::INTERFACE_TYPE::Crb,
            ]);
        let version = regs
            .intf_capability
            .read_as_enum(IntfCapability::INTERFACE_VERSION);
        let family = if ptp || version == Some(IntfCapability::INTERFACE_VERSION::Value::PtpFifo) {
            Family::Tpm20
        } else {
            Family::Tpm12
        };
        Some(Identity {
            family,
            vendor: regs.did_vid.read(DidVid::VID) as u16,
            device: regs.did_vid.read(DidVid::DID) as u16,
            revision: regs.rid.get(),
        })
    }

    /// Whether the TPM speaks the CRB interface instead of this one.
    pub fn is_crb(&self) -> bool {
        let id = self.regs(0).interface_id.extract();
        id.get() != u32::MAX && id.matches_all(InterfaceId::INTERFACE_TYPE::Crb)
    }

    /// Claim the current locality.
    pub fn request_locality(&mut self) -> Result<(), Error> {
        let regs = self.regs(self.locality);
        if regs
            .access
            .matches_all(Access::VALID::SET + Access::ACTIVE_LOCALITY::SET)
        {
            return Ok(());
        }
        regs.access.write(Access::REQUEST_USE::SET);
        if self.wait(TIMEOUT_A, |regs| {
            regs.access
                .matches_all(Access::VALID::SET + Access::ACTIVE_LOCALITY::SET)
        }) {
            Ok(())
        } else {
            Err(Error::Locality)
        }
    }

    /// Give the current locality up.
    pub fn release_locality(&mut self) {
        self.regs(self.locality)
            .access
            .write(Access::ACTIVE_LOCALITY::SET);
    }

    fn burst_count(&mut self) -> Result<usize, Error> {
        let mut count = 0;
        if self.wait(TIMEOUT_D, |regs| {
            count = usize::from(regs.burst_count[0].get())
                | usize::from(regs.burst_count[1].get()) << 8;
            count != 0
        }) {
            Ok(count)
        } else {
            Err(Error::Timeout)
        }
    }

    fn status(&mut self) -> Result<LocalRegisterCopy<u8, Sts::Register>, Error> {
        let mut status = LocalRegisterCopy::new(0);
        if self.wait(TIMEOUT_C, |regs| {
            status = regs.sts.extract();
            status.is_set(Sts::VALID)
        }) {
            Ok(status)
        } else {
            Err(Error::Timeout)
        }
    }

    fn ready(&mut self) -> Result<(), Error> {
        self.regs(self.locality).sts.write(Sts::COMMAND_READY::SET);
        if self.wait(TIMEOUT_B, |regs| regs.sts.is_set(Sts::COMMAND_READY)) {
            Ok(())
        } else {
            Err(Error::Timeout)
        }
    }

    /// Bytes the TPM accepts or offers now: the unused part of the last
    /// burst count, else a fresh one. A TPM with a static burst count only
    /// raises it again once the previous burst is used up (TIS 1.3
    /// §5.6.12), so credit carries across the transfer.
    fn credit(&mut self, credit: &mut usize) -> Result<(), Error> {
        if *credit == 0 {
            *credit = self.burst_count()?;
        }
        Ok(())
    }

    fn send(&mut self, command: &[u8]) -> Result<(), Error> {
        let Some((last, body)) = command.split_last() else {
            return Err(Error::Protocol);
        };
        let mut credit = 0;
        let mut sent = 0;
        while sent < body.len() {
            self.credit(&mut credit)?;
            let burst = credit.min(body.len() - sent);
            let fifo = &self.regs(self.locality).data_fifo;
            body[sent..sent + burst]
                .iter()
                .for_each(|&byte| fifo.set(byte));
            sent += burst;
            credit -= burst;
        }
        if !self.status()?.is_set(Sts::EXPECT) {
            return Err(Error::Protocol);
        }
        self.credit(&mut credit)?;
        self.regs(self.locality).data_fifo.set(*last);
        if self.status()?.is_set(Sts::EXPECT) {
            return Err(Error::Protocol);
        }
        self.regs(self.locality).sts.write(Sts::GO::SET);
        Ok(())
    }

    fn receive_into(&mut self, buf: &mut [u8], credit: &mut usize) -> Result<(), Error> {
        let mut received = 0;
        while received < buf.len() {
            // dataAvail must stay set until the last byte is read.
            if !self.status()?.is_set(Sts::DATA_AVAIL) {
                return Err(Error::Protocol);
            }
            self.credit(credit)?;
            let burst = (*credit).min(buf.len() - received);
            let fifo = &self.regs(self.locality).data_fifo;
            buf[received..received + burst]
                .iter_mut()
                .for_each(|byte| *byte = fifo.get());
            received += burst;
            *credit -= burst;
        }
        Ok(())
    }

    /// Read the response, asking for it again (`responseRetry`, TIS 1.3
    /// §11.2.12) when the transfer broke off or overran. The command is not
    /// resent: that would execute it, e.g. extend a PCR, twice.
    fn receive(&mut self, response: &mut [u8]) -> Result<usize, Error> {
        let data_avail = Sts::VALID::SET + Sts::DATA_AVAIL::SET;
        if !self.wait(TIMEOUT_COMMAND, |regs| regs.sts.matches_all(data_avail)) {
            return Err(Error::Timeout);
        }
        let mut result = self.receive_once(response);
        for _ in 0..RESPONSE_RETRIES {
            if !matches!(result, Err(Error::Protocol | Error::Timeout)) {
                break;
            }
            self.regs(self.locality).sts.write(Sts::RESPONSE_RETRY::SET);
            if !self.wait(TIMEOUT_C, |regs| regs.sts.matches_all(data_avail)) {
                break;
            }
            result = self.receive_once(response);
        }
        result
    }

    fn receive_once(&mut self, response: &mut [u8]) -> Result<usize, Error> {
        let mut credit = 0;
        let header = response.get_mut(..HEADER_SIZE).ok_or(Error::Response)?;
        self.receive_into(header, &mut credit)?;
        let size = u32::from_be_bytes([header[2], header[3], header[4], header[5]]) as usize;
        if size < HEADER_SIZE {
            return Err(Error::Response);
        }
        let rest = response.get_mut(HEADER_SIZE..size).ok_or(Error::Response)?;
        self.receive_into(rest, &mut credit)?;
        if self.status()?.is_set(Sts::DATA_AVAIL) {
            return Err(Error::Protocol);
        }
        Ok(size)
    }
}

impl<D: DelayNs> Transport for Tis<D> {
    fn transmit(&mut self, command: &[u8], response: &mut [u8]) -> Result<usize, Error> {
        self.request_locality()?;
        let result = self
            .ready()
            .and_then(|()| self.send(command))
            .and_then(|()| self.receive(response));
        // Back to idle: drops a partial response and readies the next command.
        self.regs(self.locality).sts.write(Sts::COMMAND_READY::SET);
        result
    }

    fn wait_ms(&mut self, ms: u32) {
        self.delay.delay_ms(ms);
    }

    fn release(&mut self) {
        self.release_locality();
    }
}
