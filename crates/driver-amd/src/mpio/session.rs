// SPDX-License-Identifier: MIT OR Apache-2.0
//! One retained MPIO controller, NOT one PCI root. The caller must bind
//! aliases first. Backing RAM is permanently reserved, including on error.

use super::{
    ancillary::Ancillary,
    mailbox::{self, Mailbox, Poll},
    rs::{self, Ask, Port},
};
use crate::{dma::Window, smn::SmnAccess};
use core::num::NonZeroU32;
use embedded_hal::delay::DelayNs;
use heapless::Vec;
use tock_registers::{LocalRegisterCopy, interfaces::Readable, register_bitfields};
use zerocopy::FromBytes;

pub const MAX_PORTS: usize = 32;
/// Oversized receive allocation with separate ancillary storage and guards.
/// Firmware gets no allocation length: never use a narrowly sized stack buffer.
pub const DMA_SIZE: usize = 0x10000;
const ANCILLARY_OFFSET: usize = 0x8000;
const ASK_BYTES: usize = size_of::<Ask>();
pub type Dma = Window<DMA_SIZE>;
pub type CacheSync = unsafe fn(*const u8, usize);

register_bitfields![u32,
    Global [
        SKIP_VETTING OFFSET(0) NUMBITS(1) [],
        USE_PHY_SRAM OFFSET(6) NUMBITS(1) [],
        VALID_PHY_FW OFFSET(7) NUMBITS(1) []
    ],
    Transfer [
        SELECTED OFFSET(0) NUMBITS(1) [],
        TO_MPIO OFFSET(1) NUMBITS(1) []
    ]
];

#[derive(Clone, Copy)]
pub struct Config<'a> {
    pub port: Port,
    pub phy: &'a [u32],
    pub speed: &'a [u32],
}

pub struct Prepared {
    asks: Vec<Ask, MAX_PORTS>,
    ancillary: Ancillary,
    psp_vetting: bool,
    encoding: mailbox::Encoding,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Empty,
    Capacity,
    Overlap,
    Unsupported,
    Buffer,
    State,
    Lanes(rs::Error),
    Ancillary(super::ancillary::Error),
    Mailbox(mailbox::Error),
    Receipt(u16, u32),
    Guard,
}
impl From<mailbox::Error> for Error {
    fn from(e: mailbox::Error) -> Self {
        Self::Mailbox(e)
    }
}

impl Prepared {
    /// Complete preflight: no mailbox or DMA access, no silently lost port.
    pub fn new(instance: u8, roots: &[u8], ports: &[Config<'_>]) -> Result<Self, Error> {
        if ports.is_empty() {
            return Err(Error::Empty);
        }
        if ports.len() > MAX_PORTS {
            return Err(Error::Capacity);
        }
        let mut result = Self {
            asks: Vec::new(),
            ancillary: Ancillary::new(instance),
            psp_vetting: true,
            encoding: mailbox::Encoding::Retained,
        };
        let mut used = [false; 136];
        for config in ports {
            // This owner has no asynchronous hotplug/interrupt service yet.
            // Do not accept a policy whose background DMA it cannot manage.
            if config.port.hotplug != 0 {
                return Err(Error::Unsupported);
            }
            // Validate the full lane range before indexing the occupancy map.
            rs::translate(roots, config.port.start, config.port.end).map_err(Error::Lanes)?;
            for lane in
                config.port.start.min(config.port.end)..=config.port.start.max(config.port.end)
            {
                if core::mem::replace(&mut used[usize::from(lane)], true) {
                    return Err(Error::Overlap);
                }
            }
            let index = result
                .ancillary
                .add(&config.port, config.phy, config.speed)
                .map_err(Error::Ancillary)?;
            let (_, ask) = rs::build(roots, &config.port, index).map_err(Error::Lanes)?;
            result.asks.push(ask).map_err(|_| Error::Capacity)?;
        }
        Ok(result)
    }
    /// Explicit firmware-policy selection. Siena's donor enables vetting;
    /// public Onyx sets CfgSkipPspMessage=1. This is not an error fallback.
    pub fn with_psp_vetting(mut self, enabled: bool) -> Self {
        self.psp_vetting = enabled;
        self
    }
    pub fn with_encoding(mut self, encoding: mailbox::Encoding) -> Self {
        self.encoding = encoding;
        self
    }
}

/// Completed protocol phase, NOT a claim that individual links trained.
/// Inspect each returned ASK's low status nibble and actual device visibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Fresh,
    Submitted,
    Mapped,
    Configured,
    Perst,
    Trained,
    Updated,
    Poisoned,
}

pub struct Session<A> {
    smn: A,
    mailbox: Mailbox,
    dma: Dma,
    prepared: Prepared,
    sync: CacheSync,
    state: State,
    idle_reads: NonZeroU32,
}

impl<A: SmnAccess> Session<A> {
    /// # Safety
    /// BSP-exclusive ownership of the entire controller and ALL alias views,
    /// not just this root path. No prior unresolved operation. The backing
    /// allocation remains reserved until reset, even if this object is dropped
    /// or an operation times out. `sync` must publish CPU writes AND invalidate
    /// stale CPU lines after device completion; compiler fences are not enough.
    /// The retained firmware/PHY images and this Siena ABI must be verified.
    pub unsafe fn new(
        smn: A,
        dma: Dma,
        prepared: Prepared,
        sync: CacheSync,
        poll: Poll,
        idle_reads: NonZeroU32,
    ) -> Result<Self, Error> {
        if dma.len() != DMA_SIZE
            || dma.phys(0) == 0
            || dma.phys(0) & 63 != 0
            || dma.as_ptr() as usize & 63 != 0
        {
            return Err(Error::Buffer);
        }
        Ok(Self {
            smn,
            mailbox: unsafe { Mailbox::new_with_encoding(poll, prepared.encoding) },
            dma,
            prepared,
            sync,
            state: State::Fresh,
            idle_reads,
        })
    }
    pub fn state(&self) -> State {
        self.state
    }
    pub fn asks(&self) -> Result<&[Ask], Error> {
        match self.state {
            State::Mapped | State::Configured | State::Perst | State::Trained | State::Updated => {
                Ok(&self.prepared.asks)
            }
            _ => Err(Error::State),
        }
    }
    fn require(&self, expected: State) -> Result<(), Error> {
        if self.state == expected {
            Ok(())
        } else {
            Err(Error::State)
        }
    }
    fn cache(&self) {
        // SAFETY: constructor's allocation and cache-coherency contract.
        unsafe { (self.sync)(self.dma.as_ptr(), DMA_SIZE) };
    }
    fn guards(&self) -> Result<(), Error> {
        let ask_end = self.prepared.asks.len() * ASK_BYTES;
        let anc_end = ANCILLARY_OFFSET + self.prepared.ancillary.bytes().len();
        let mut bytes = [0u8; 64];
        for offset in (0..DMA_SIZE).step_by(bytes.len()) {
            self.dma.read(offset, &mut bytes);
            for (i, &byte) in bytes.iter().enumerate() {
                let at = offset + i;
                if at >= ask_end && !(ANCILLARY_OFFSET..anc_end).contains(&at) && byte != 0xa5 {
                    return Err(Error::Guard);
                }
            }
        }
        Ok(())
    }
    fn exchange(
        &mut self,
        command: u16,
        args: [u32; 6],
        delay: &mut impl DelayNs,
    ) -> Result<[u32; 6], Error> {
        // Fail closed throughout a compound operation. Neither a rejection,
        // timeout nor malformed DMA result grants permission to reuse RAM or
        // construct a fresh controller session. No cache reads on error.
        self.state = State::Poisoned;
        self.cache();
        fstart_log::info!(
            "mpio: cmd={:#x} args=[{:#x},{:#x},{:#x},{:#x},{:#x},{:#x}]",
            command,
            args[0],
            args[1],
            args[2],
            args[3],
            args[4],
            args[5]
        );
        let reply = unsafe { self.mailbox.request(&self.smn, command, args, delay) }
            .map_err(|error| self.failure(command, "request", error))?;
        fstart_log::info!(
            "mpio: cmd={:#x} reply=[{:#x},{:#x},{:#x},{:#x},{:#x},{:#x}]",
            command,
            reply[0],
            reply[1],
            reply[2],
            reply[3],
            reply[4],
            reply[5]
        );
        self.mailbox
            .wait_idle(&self.smn, delay, self.idle_reads)
            .map_err(|error| self.failure(command, "idle", error))?;
        self.cache();
        self.guards()?;
        Ok(reply)
    }
    fn failure(&self, command: u16, stage: &str, error: mailbox::Error) -> Error {
        let name = match error {
            mailbox::Error::ReadyTimeout => "ready-timeout",
            mailbox::Error::CompletionTimeout => "completion-timeout",
            mailbox::Error::IdleTimeout => "idle-timeout",
            mailbox::Error::Unavailable => "unavailable",
            mailbox::Error::Rejected(_) => "rejected",
            _ => "protocol",
        };
        fstart_log::error!(
            "mpio: cmd={:#x} {} {} control={:#x}; NO retry/DMA release",
            command,
            stage,
            name,
            self.smn.reg(mailbox::CONTROL).get()
        );
        for i in 0..6 {
            fstart_log::error!(
                "mpio: pending arg{}={:#x}",
                i,
                self.smn.reg(mailbox::ARGUMENTS.offset(i * 4)).get()
            );
        }
        Error::Mailbox(error)
    }
    fn send_asks(&mut self, delay: &mut impl DelayNs) -> Result<(), Error> {
        for (i, ask) in self.prepared.asks.iter().enumerate() {
            self.dma.write(i * ASK_BYTES, ask.bytes());
        }
        let mut flags = LocalRegisterCopy::<u32, Transfer::Register>::new(0);
        flags.write(Transfer::SELECTED::SET + Transfer::TO_MPIO::SET);
        let reply = self.exchange(
            mailbox::msg::TRANSFER_ASK,
            [
                0,
                self.dma.phys(0),
                flags.get(),
                0,
                self.prepared.asks.len() as u32,
                0,
            ],
            delay,
        )?;
        if reply[0] != 1 {
            return Err(Error::Receipt(mailbox::msg::TRANSFER_ASK, reply[0]));
        }
        Ok(())
    }
    /// Conservative defaults plus the explicitly selected PSP-vetting policy.
    /// Power gating, save/restore and loopback remain off.
    pub fn submit(&mut self, delay: &mut impl DelayNs) -> Result<(), Error> {
        self.require(State::Fresh)?;
        self.state = State::Poisoned;
        fstart_log::info!("mpio: acquire initial idle");
        self.mailbox
            .wait_idle(&self.smn, delay, self.idle_reads)
            .map_err(|error| self.failure(1, "initial idle", error))?;
        for offset in (0..DMA_SIZE).step_by(64) {
            self.dma.write(offset, &[0xa5; 64]);
        }
        // Populate BOTH buffers before any operation can expose a pointer.
        for (i, ask) in self.prepared.asks.iter().enumerate() {
            self.dma.write(i * ASK_BYTES, ask.bytes());
        }
        self.dma
            .write(ANCILLARY_OFFSET, self.prepared.ancillary.bytes());
        let mut global = LocalRegisterCopy::<u32, Global::Register>::new(0);
        global.write(
            Global::USE_PHY_SRAM::SET
                + Global::VALID_PHY_FW::SET
                + Global::SKIP_VETTING.val(u32::from(!self.prepared.psp_vetting)),
        );
        self.exchange(
            mailbox::msg::GLOBAL_CONFIG,
            [global.get(), 0, 0, 0, 0, 0],
            delay,
        )?;
        if self.prepared.ancillary.dwords() != 0 {
            self.exchange(
                mailbox::msg::EXT_ATTRIBUTES,
                [
                    0,
                    self.dma.phys(ANCILLARY_OFFSET),
                    self.prepared.ancillary.dwords(),
                    0,
                    0,
                    0,
                ],
                delay,
            )?;
        }
        self.send_asks(delay)?;
        self.state = State::Submitted;
        Ok(())
    }
    /// Execute exactly the next phase, then synchronously retrieve its ASK
    /// snapshot. The caller must bind map results/program port remaps before
    /// Configure; after Perst it must release physical PERST and wait for the
    /// board's required stable reset/refclock interval before Train.
    pub fn advance(&mut self, delay: &mut impl DelayNs) -> Result<(), Error> {
        let (phase, next) = match self.state {
            State::Submitted => (mailbox::phase::MAP, State::Mapped),
            State::Mapped => (mailbox::phase::CONFIGURE, State::Configured),
            State::Configured => (mailbox::phase::PERST, State::Perst),
            State::Perst => (mailbox::phase::TRAIN, State::Trained),
            _ => return Err(Error::State),
        };
        self.exchange(mailbox::msg::SETUP_LINK, [phase, 0, 0, 0, 0, 0], delay)?;
        // NFX's msg3 consumer ignores reply argument0; completion is the
        // transport status plus idle. Never reinterpret it as a byte count.
        self.exchange(
            mailbox::msg::GET_ASK,
            [0, self.dma.phys(0), 0, 0, 0, 0],
            delay,
        )?;
        for (i, ask) in self.prepared.asks.iter_mut().enumerate() {
            let mut bytes = [0; ASK_BYTES];
            self.dma.read(i * ASK_BYTES, &mut bytes);
            *ask = Ask::read_from_bytes(&bytes).map_err(|_| Error::Buffer)?;
            if ask.kind().is_none() {
                return Err(Error::Buffer);
            }
        }
        self.state = next;
        Ok(())
    }
    /// IDs must come from actual trained-result/device-map binding, NOT
    /// from requested devfn or fuse-filtered root ordinals. SATA has no LBID.
    pub fn update(&mut self, ids: &[Option<u8>], delay: &mut impl DelayNs) -> Result<(), Error> {
        self.require(State::Trained)?;
        if ids.len() != self.prepared.asks.len() {
            return Err(Error::Buffer);
        }
        let mut updated = self.prepared.asks.clone();
        for (ask, id) in updated.iter_mut().zip(ids) {
            if let Some(id) = id {
                if ask.kind() != Some(rs::Kind::Pcie) {
                    return Err(Error::Buffer);
                }
                ask.set_logical_bridge(*id).map_err(Error::Lanes)?;
            }
        }
        self.prepared.asks = updated;
        self.send_asks(delay)?;
        self.exchange(mailbox::msg::UPDATE_LINK, [0; 6], delay)?;
        self.state = State::Updated;
        Ok(())
    }
}

#[cfg(test)]
#[path = "session_tests.rs"]
mod tests;
