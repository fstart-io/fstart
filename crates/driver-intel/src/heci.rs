// SPDX-License-Identifier: MIT OR Apache-2.0
//! Shared HECI ring transport.
//! No heap, automatic retransmission, platform policy, or system reset. After a
//! partial/error transaction the interface is poisoned until explicit reset.
use embedded_hal::delay::DelayNs;
use fstart_core::mmio::MmioRegister;
use tock_registers::interfaces::{Readable, Writeable};
use tock_registers::{LocalRegisterCopy, register_bitfields};

register_bitfields![u32,
    pub Csr [
        IE OFFSET(0) NUMBITS(1) [],
        IS OFFSET(1) NUMBITS(1) [],
        IG OFFSET(2) NUMBITS(1) [],
        READY OFFSET(3) NUMBITS(1) [],
        RESET OFFSET(4) NUMBITS(1) [],
        RP OFFSET(8) NUMBITS(8) [],
        WP OFFSET(16) NUMBITS(8) [],
        DEPTH OFFSET(24) NUMBITS(8) []
    ],
    pub Header [
        CLIENT OFFSET(0) NUMBITS(8) [],
        HOST OFFSET(8) NUMBITS(8) [],
        LENGTH OFFSET(16) NUMBITS(9) [],
        RESERVED OFFSET(25) NUMBITS(6) [],
        COMPLETE OFFSET(31) NUMBITS(1) []
    ],
    pub Fifo [ WORD OFFSET(0) NUMBITS(32) [] ]
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    InvalidBudget,
    InvalidInput,
    Geometry,
    NotReady,
    ResetRequested,
    Timeout,
    Header,
    Address,
    Capacity { needed: usize, capacity: usize },
    Incomplete,
    Stale,
    NeedsReset,
}

/// Number of delays, with a sample before the first and after the final delay.
#[derive(Debug, Clone, Copy)]
pub struct Budgets {
    ready: u32,
    io: u32,
    fragments: u16,
}
impl Budgets {
    /// Poll interval is the source's 100 us; maxima are 15 s ready / 5 s I/O.
    pub fn new(ready: u32, io: u32, fragments: u16) -> Result<Self, Error> {
        if ready == 0 || ready > 150_000 || io == 0 || io > 50_000 || fragments == 0 {
            return Err(Error::InvalidBudget);
        }
        Ok(Self {
            ready,
            io,
            fragments,
        })
    }
    pub const SOURCE: Self = Self {
        ready: 150_000,
        io: 50_000,
        fragments: 1024,
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ring {
    pub depth: u8,
    pub filled: u8,
}
impl Ring {
    pub fn decode(raw: u32) -> Result<Self, Error> {
        let r = LocalRegisterCopy::<u32, Csr::Register>::new(raw);
        let depth = r.read(Csr::DEPTH) as u8;
        let filled = (r.read(Csr::WP) as u8).wrapping_sub(r.read(Csr::RP) as u8);
        if depth == 0 || filled > depth {
            return Err(Error::Geometry);
        }
        Ok(Self { depth, filled })
    }
    fn empty(self) -> usize {
        usize::from(self.depth - self.filled)
    }
}

pub type MmioTransport = Transport<
    MmioRegister<u32, Csr::Register>,
    MmioRegister<u32, Csr::Register>,
    MmioRegister<u32, Fifo::Register>,
    MmioRegister<u32, Fifo::Register>,
>;

/// Endpoints are standard Tock registers, including side-effecting FIFO ports.
/// The owner must serialize the complete transaction (including platform gate).
pub struct Transport<H, M, W, R> {
    host: H,
    me: M,
    tx: W,
    rx: R,
    budgets: Budgets,
    poisoned: bool,
}
impl MmioTransport {
    /// # Safety
    /// Own the enabled, mapped, stable 4 KiB HECI BAR exclusively until this
    /// transport is dropped. `base` is the validated PCI BAR, not physical RAM.
    pub unsafe fn at(base: usize, budgets: Budgets) -> Result<Self, Error> {
        if base == 0 || base & 0xfff != 0 || base.checked_add(0xfff).is_none() {
            return Err(Error::InvalidInput);
        }
        Ok(Self::new(
            unsafe { MmioRegister::new(base + 4) },
            unsafe { MmioRegister::new(base + 12) },
            unsafe { MmioRegister::new(base) },
            unsafe { MmioRegister::new(base + 8) },
            budgets,
        ))
    }
}
impl<H, M, W, R> Transport<H, M, W, R>
where
    H: Readable<T = u32, R = Csr::Register> + Writeable<T = u32, R = Csr::Register>,
    M: Readable<T = u32, R = Csr::Register>,
    W: Writeable<T = u32, R = Fifo::Register>,
    R: Readable<T = u32, R = Fifo::Register>,
{
    pub fn new(host: H, me: M, tx: W, rx: R, budgets: Budgets) -> Self {
        Self {
            host,
            me,
            tx,
            rx,
            budgets,
            poisoned: false,
        }
    }
    /// Validate both geometries without writing or consuming FIFO data.
    pub fn geometry(&self) -> Result<(Ring, Ring), Error> {
        Ok((Ring::decode(self.host.get())?, Ring::decode(self.me.get())?))
    }
    fn host_update(&self, clear: u32, set: u32, acknowledge: bool) {
        // IS is W1C: ordinary writes must never echo a sampled pending bit.
        let mut value = self.host.get() & !(clear | Csr::IS::SET.value);
        value |= set;
        if acknowledge {
            value |= Csr::IS::SET.value;
        }
        self.host.set(value);
    }
    fn me_ready(&self) -> Result<(), Error> {
        let csr = self.me.get();
        Ring::decode(csr)?;
        if csr & Csr::RESET::SET.value != 0 {
            return Err(Error::ResetRequested);
        }
        if csr & Csr::READY::SET.value == 0 {
            return Err(Error::NotReady);
        }
        Ok(())
    }
    fn wait(
        &self,
        delays: u32,
        delay: &mut impl DelayNs,
        mut sample: impl FnMut() -> Result<bool, Error>,
    ) -> Result<(), Error> {
        for index in 0..=delays {
            if sample()? {
                return Ok(());
            }
            if index != delays {
                delay.delay_us(100);
            }
        }
        Err(Error::Timeout)
    }
    /// Native initial bring-up is not an arbitrary reset of a ready interface.
    /// Platform must validate ownership/firmware policy before this call.
    pub fn initialize(&mut self, delay: &mut impl DelayNs) -> Result<(), Error> {
        self.geometry()?;
        if self.poisoned {
            return Err(Error::NeedsReset);
        }
        if self.me.get() & Csr::RESET::SET.value != 0 {
            return self.reset(delay);
        }
        self.wait(self.budgets.ready, delay, || {
            let csr = self.me.get();
            Ring::decode(csr)?;
            if csr & Csr::RESET::SET.value != 0 {
                return Err(Error::ResetRequested);
            }
            Ok(csr & Csr::READY::SET.value != 0)
        })?;
        if self.host.get() & Csr::READY::SET.value == 0 {
            self.host_update(
                Csr::RESET::SET.value,
                Csr::IG::SET.value | Csr::READY::SET.value,
                false,
            );
        }
        self.check_ready()
    }
    /// Interface reset handshake only; never resets or powers the machine.
    pub fn reset(&mut self, delay: &mut impl DelayNs) -> Result<(), Error> {
        self.geometry()?;
        self.poisoned = true;
        if self.host.get() & Csr::RESET::SET.value == 0 {
            self.host_update(0, Csr::RESET::SET.value | Csr::IG::SET.value, false);
        }
        self.wait(self.budgets.ready, delay, || {
            Ok(self.host.get() & Csr::READY::SET.value == 0)
        })?;
        self.wait(self.budgets.ready, delay, || {
            let csr = self.me.get();
            Ring::decode(csr)?;
            Ok(csr & Csr::READY::SET.value != 0)
        })?;
        self.wait(self.budgets.ready, delay, || {
            Ok(self.host.get() & Csr::IS::SET.value != 0)
        })?;
        self.host_update(
            Csr::RESET::SET.value,
            Csr::IG::SET.value | Csr::READY::SET.value,
            true,
        );
        // Native ME may deassert RESET only after host releases the handshake.
        self.me_ready()?;
        let (host, me) = self.geometry()?;
        if host.filled != 0 || me.filled != 0 {
            return Err(Error::Stale);
        }
        self.poisoned = false;
        Ok(())
    }
    /// Non-destructive readiness check, including poison and both geometries.
    pub fn ready(&self) -> Result<(), Error> {
        self.check_ready()
    }
    fn check_ready(&self) -> Result<(), Error> {
        if self.poisoned {
            return Err(Error::NeedsReset);
        }
        self.me_ready()?;
        let host = self.host.get();
        Ring::decode(host)?;
        if host & Csr::RESET::SET.value != 0 {
            return Err(Error::ResetRequested);
        }
        if host & Csr::READY::SET.value == 0 {
            return Err(Error::NotReady);
        }
        Ok(())
    }
    fn wait_slots(
        &self,
        slots: usize,
        receive: bool,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        self.wait(self.budgets.io, delay, || {
            self.check_ready()?;
            let ring = Ring::decode(if receive {
                self.me.get()
            } else {
                self.host.get()
            })?;
            if slots > usize::from(ring.depth) {
                return Err(Error::Geometry);
            }
            Ok(if receive {
                usize::from(ring.filled) >= slots
            } else {
                ring.empty() >= slots
            })
        })
    }
    /// Source fragmentation limit: min(depth*4, 511) - one header DWORD.
    /// No automatic retry: replaying a partially delivered command is unsafe.
    pub fn send(
        &mut self,
        bytes: &[u8],
        host: u8,
        client: u8,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        if bytes.is_empty() {
            return Err(Error::InvalidInput);
        }
        self.check_ready()?;
        let ring = Ring::decode(self.host.get())?;
        if ring.depth < 2 {
            return Err(Error::Geometry);
        }
        let max = (usize::from(ring.depth) * 4).min(511) - 4;
        if bytes.len().div_ceil(max) > usize::from(self.budgets.fragments) {
            return Err(Error::InvalidInput);
        }
        let result = self.send_inner(bytes, host, client, max, delay);
        if result.is_err() {
            self.poisoned = true;
        }
        result
    }
    fn send_inner(
        &self,
        bytes: &[u8],
        host: u8,
        client: u8,
        max: usize,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        self.host_update(0, 0, true);
        let mut offset = 0;
        for fragment in bytes.chunks(max) {
            self.wait_slots(1 + fragment.len().div_ceil(4), false, delay)?;
            let complete = offset + fragment.len() == bytes.len();
            let header = Header::CLIENT.val(u32::from(client))
                + Header::HOST.val(u32::from(host))
                + Header::LENGTH.val(fragment.len() as u32)
                + Header::COMPLETE.val(complete as u32);
            self.tx.set(header.value);
            for chunk in fragment.chunks(4) {
                let mut word = [0u8; 4];
                word[..chunk.len()].copy_from_slice(chunk);
                self.tx.set(u32::from_le_bytes(word));
            }
            self.host_update(0, Csr::IG::SET.value, false);
            self.me_ready()?;
            offset += fragment.len();
        }
        Ok(())
    }
    /// Concatenates exact byte lengths. On error buffer contents are unpublished
    /// partial data, never a response; reset is mandatory before another call.
    pub fn receive(
        &mut self,
        buffer: &mut [u8],
        host: u8,
        client: u8,
        delay: &mut impl DelayNs,
    ) -> Result<usize, Error> {
        if buffer.is_empty() {
            return Err(Error::InvalidInput);
        }
        // A split-API request may already have been delivered. Readiness and
        // geometry failures therefore require reset just like FIFO failures.
        let result = self
            .check_ready()
            .and_then(|()| self.receive_inner(buffer, host, client, delay));
        if result.is_err() {
            self.poisoned = true;
        }
        result
    }
    fn receive_inner(
        &self,
        buffer: &mut [u8],
        host: u8,
        client: u8,
        delay: &mut impl DelayNs,
    ) -> Result<usize, Error> {
        self.host_update(0, 0, true);
        let mut total = 0usize;
        for _ in 0..self.budgets.fragments {
            self.wait_slots(1, true, delay)?;
            let header = LocalRegisterCopy::<u32, Header::Register>::new(self.rx.get());
            if header.read(Header::RESERVED) != 0 {
                return Err(Error::Header);
            }
            if header.read(Header::CLIENT) != u32::from(client)
                || header.read(Header::HOST) != u32::from(host)
            {
                return Err(Error::Address);
            }
            let length = header.read(Header::LENGTH) as usize;
            let complete = header.is_set(Header::COMPLETE);
            if length == 0 && (!complete || total != 0) {
                return Err(Error::Incomplete);
            }
            let needed = total.checked_add(length).ok_or(Error::InvalidInput)?;
            if needed > buffer.len() {
                return Err(Error::Capacity {
                    needed,
                    capacity: buffer.len(),
                });
            }
            let slots = length.div_ceil(4);
            // Header occupies one ring slot in the source's fragment geometry.
            if slots + 1 > usize::from(Ring::decode(self.me.get())?.depth) {
                return Err(Error::Header);
            }
            self.wait_slots(slots, true, delay)?;
            for index in 0..slots {
                let word = self.rx.get().to_le_bytes();
                let count = (length - index * 4).min(4);
                buffer[total + index * 4..total + index * 4 + count]
                    .copy_from_slice(&word[..count]);
            }
            self.me_ready()?;
            self.host_update(0, Csr::IG::SET.value, false);
            total = needed;
            if complete {
                return Ok(total);
            }
            if total == buffer.len() {
                return Err(Error::Incomplete);
            }
        }
        Err(Error::Incomplete)
    }
    /// Reject queued unsolicited/stale responses before transmitting a request.
    pub fn send_receive(
        &mut self,
        request: &[u8],
        response: &mut [u8],
        host: u8,
        client: u8,
        delay: &mut impl DelayNs,
    ) -> Result<usize, Error> {
        if response.is_empty() || request.is_empty() {
            return Err(Error::InvalidInput);
        }
        self.check_ready()?;
        if Ring::decode(self.me.get())?.filled != 0 {
            return Err(Error::Stale);
        }
        self.send(request, host, client, delay)?;
        let result = self.receive(response, host, client, delay);
        // A request is already in flight even if receive's initial readiness
        // check fails before touching its FIFO. Do not permit command replay.
        if result.is_err() {
            self.poisoned = true;
        }
        result
    }
}

#[cfg(test)]
#[path = "heci_tests.rs"]
pub(crate) mod tests;
