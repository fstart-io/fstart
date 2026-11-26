//! Data Object Exchange (PCIe 6.0 §6.30, §7.9.24): the config-space mailbox
//! CXL devices answer CDAT reads through ([`super::cdat`]).
//!
//! One exchange: abort a mailbox that reports an error, refuse a busy one,
//! write the request dwords, set Go, wait for Data Object Ready, then read
//! the response a dword at a time, acknowledging each with a write to the
//! read mailbox (the CXL 3.1 sequence, as the vendors' `CxlDoeExchange`:
//! 1 ms polls, 1000 tries).
//!
//! The mailbox goes through [`ConfigRegionAccess`], so a host model can
//! stand in for the device.

use pci_types::{ConfigRegionAccess, PciAddress};
use tock_registers::{LocalRegisterCopy, register_bitfields};

use fstart_core::services::Timer;

/// The DOE extended capability ID.
pub const EXT_CAP_ID: u16 = 0x2e;
/// DOE Capabilities (+0x04, 32-bit).
pub const CAPABILITIES: u16 = 0x04;
/// DOE Control (+0x08, 32-bit), [`Control`].
pub const CONTROL: u16 = 0x08;
/// DOE Status (+0x0C, 32-bit), [`Status`].
pub const STATUS: u16 = 0x0c;
/// DOE Write Data Mailbox (+0x10, 32-bit).
pub const WRITE: u16 = 0x10;
/// DOE Read Data Mailbox (+0x14, 32-bit); a write acknowledges a dword.
pub const READ: u16 = 0x14;

/// How long one poll for the mailbox waits (1 ms × 1000).
pub const TIMEOUT_US: u64 = 1_000_000;
/// The longest data object (2^18 dwords, the length field's 0).
pub const MAX_DWORDS: usize = 1 << 18;

/// The PCI-SIG's vendor ID of the discovery protocol.
pub const PCI_SIG: u16 = 0x0001;
/// Discovery (data object type 0 of [`PCI_SIG`]).
pub const DISCOVERY: u8 = 0;

register_bitfields![u32,
    /// DOE Control +0x08 (32-bit).
    pub Control [
        ABORT OFFSET(0) NUMBITS(1) [],
        INTERRUPT_ENABLE OFFSET(1) NUMBITS(1) [],
        GO OFFSET(31) NUMBITS(1) []
    ],
    /// DOE Status +0x0C (32-bit).
    pub Status [
        BUSY OFFSET(0) NUMBITS(1) [],
        INTERRUPT OFFSET(1) NUMBITS(1) [],
        ERROR OFFSET(2) NUMBITS(1) [],
        AT_ATTENTION OFFSET(3) NUMBITS(1) [],
        DATA_OBJECT_READY OFFSET(31) NUMBITS(1) []
    ],
    /// Data object header 1 (dword 0).
    pub Header1 [
        VENDOR OFFSET(0) NUMBITS(16) [],
        TYPE OFFSET(16) NUMBITS(8) []
    ],
    /// Data object header 2 (dword 1): the object's length in dwords,
    /// headers included (0: 2^18).
    pub Header2 [
        LENGTH OFFSET(0) NUMBITS(18) []
    ]
];

/// Why an exchange failed (AMD's codes: [`Error::code`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// No Data Object Ready in time (`0x434C_0601`).
    Timeout,
    /// The mailbox stayed busy (`0x434C_0602`).
    Busy,
    /// The mailbox reported an error; it was aborted.
    Aborted,
    /// A request longer than [`MAX_DWORDS`] or shorter than its headers.
    Request,
    /// A response shorter than its headers or than it said.
    Response,
}

impl Error {
    /// The vendor's error code (`0x434C_06xx`, CXL.md §2.12).
    pub const fn code(self) -> u32 {
        match self {
            Self::Timeout => 0x434c_0601,
            Self::Busy => 0x434c_0602,
            _ => 0x434c_0600,
        }
    }
}

/// The two header dwords of a data object of `vendor`/`kind` of `dwords`
/// dwords (headers included).
pub fn header(vendor: u16, kind: u8, dwords: usize) -> [u32; 2] {
    let mut h1 = LocalRegisterCopy::<u32, Header1::Register>::new(0);
    h1.write(Header1::VENDOR.val(vendor.into()) + Header1::TYPE.val(kind.into()));
    let length = if dwords >= MAX_DWORDS {
        0
    } else {
        dwords as u32
    };
    [h1.get(), length]
}

/// One DOE instance of a function.
pub struct Doe<'a, C> {
    pci: &'a C,
    function: PciAddress,
    cap: u16,
}

impl<'a, C: ConfigRegionAccess> Doe<'a, C> {
    /// The DOE capability at `cap` of `function`.
    ///
    /// # Safety
    /// `function` is present, the caller owns its config space, and `cap`
    /// is the offset of one of its DOE capabilities.
    pub const unsafe fn new(pci: &'a C, function: PciAddress, cap: u16) -> Self {
        Self { pci, function, cap }
    }

    fn read(&self, offset: u16) -> u32 {
        // SAFETY: the constructor's contract: the capability's registers.
        unsafe { self.pci.read(self.function, self.cap + offset) }
    }

    fn write(&self, offset: u16, value: u32) {
        // SAFETY: as `read`.
        unsafe { self.pci.write(self.function, self.cap + offset, value) }
    }

    fn status(&self) -> LocalRegisterCopy<u32, Status::Register> {
        LocalRegisterCopy::new(self.read(STATUS))
    }

    fn abort(&self, timer: &dyn Timer) -> Result<(), Error> {
        let mut control = LocalRegisterCopy::<u32, Control::Register>::new(0);
        control.write(Control::ABORT::SET);
        self.write(CONTROL, control.get());
        fstart_core::services::timer::poll_until(timer, TIMEOUT_US, || {
            (!self.status().is_set(Status::ERROR)).then_some(())
        })
        .ok_or(Error::Timeout)
    }

    /// Send `request` (headers included) and read the response into
    /// `response`: its length in dwords, headers included. Dwords beyond
    /// `response` are read and dropped.
    pub fn exchange(
        &self,
        request: &[u32],
        response: &mut [u32],
        timer: &dyn Timer,
    ) -> Result<usize, Error> {
        if request.len() < 2 || request.len() > MAX_DWORDS {
            return Err(Error::Request);
        }
        if self.status().is_set(Status::ERROR) {
            self.abort(timer)?;
        }
        if self.status().is_set(Status::BUSY) {
            return Err(Error::Busy);
        }
        for &dword in request {
            self.write(WRITE, dword);
        }
        let mut go = LocalRegisterCopy::<u32, Control::Register>::new(0);
        go.write(Control::GO::SET);
        self.write(CONTROL, go.get());
        let ready = fstart_core::services::timer::poll_until(timer, TIMEOUT_US, || {
            let status = self.status();
            if status.is_set(Status::ERROR) {
                Some(Err(Error::Aborted))
            } else {
                status.is_set(Status::DATA_OBJECT_READY).then_some(Ok(()))
            }
        });
        match ready {
            None => return Err(Error::Timeout),
            Some(Err(error)) => {
                let _ = self.abort(timer);
                return Err(error);
            }
            Some(Ok(())) => {}
        }
        let next = |i: usize, response: &mut [u32]| {
            let dword = self.read(READ);
            self.write(READ, 0);
            if let Some(slot) = response.get_mut(i) {
                *slot = dword;
            }
            dword
        };
        next(0, response);
        let length = match LocalRegisterCopy::<u32, Header2::Register>::new(next(1, response))
            .read(Header2::LENGTH) as usize
        {
            0 => MAX_DWORDS,
            n => n,
        };
        if length < 2 {
            return Err(Error::Response);
        }
        for i in 2..length {
            next(i, response);
        }
        Ok(length)
    }

    /// Discovery entry `index`: the protocol's `(vendor, type)` and the
    /// next index (0 after the last).
    pub fn discover(&self, index: u8, timer: &dyn Timer) -> Result<(u16, u8, u8), Error> {
        let [h1, h2] = header(PCI_SIG, DISCOVERY, 3);
        let mut response = [0u32; 3];
        if self.exchange(&[h1, h2, index.into()], &mut response, timer)? < 3 {
            return Err(Error::Response);
        }
        let [_, _, entry] = response;
        Ok((entry as u16, (entry >> 16) as u8, (entry >> 24) as u8))
    }

    /// Whether the instance serves protocol `(vendor, kind)` (the vendor's
    /// `CxlFindCdatMailbox`: discovery until the protocol or the end).
    pub fn supports(&self, vendor: u16, kind: u8, timer: &dyn Timer) -> Result<bool, Error> {
        let mut index = 0u8;
        // At most 256 entries: the index is a byte.
        for _ in 0..=u8::MAX as usize {
            let (v, k, next) = self.discover(index, timer)?;
            if (v, k) == (vendor, kind) {
                return Ok(true);
            }
            if next == 0 {
                break;
            }
            index = next;
        }
        Ok(false)
    }
}

/// A host model of a DOE mailbox for tests: requests are answered by
/// `respond`, which gets the request dwords.
#[cfg(test)]
pub(crate) mod testing {
    extern crate std;
    use super::*;
    use core::cell::RefCell;
    use std::vec::Vec;

    pub struct Model<F> {
        pub cap: u16,
        pub respond: F,
        pub state: RefCell<State>,
    }

    #[derive(Default)]
    pub struct State {
        pub request: Vec<u32>,
        pub response: Vec<u32>,
        pub at: usize,
        pub error: bool,
        pub exchanges: usize,
    }

    impl<F: Fn(&[u32]) -> Vec<u32>> Model<F> {
        pub fn new(cap: u16, respond: F) -> Self {
            Self {
                cap,
                respond,
                state: RefCell::default(),
            }
        }
    }

    impl<F: Fn(&[u32]) -> Vec<u32>> ConfigRegionAccess for Model<F> {
        unsafe fn read(&self, _: PciAddress, offset: u16) -> u32 {
            let s = self.state.borrow();
            match offset.wrapping_sub(self.cap) {
                STATUS => (u32::from(s.error) << 2) | (u32::from(s.at < s.response.len()) << 31),
                READ => s.response.get(s.at).copied().unwrap_or(0),
                _ => 0,
            }
        }

        unsafe fn write(&self, _: PciAddress, offset: u16, value: u32) {
            let mut s = self.state.borrow_mut();
            match offset.wrapping_sub(self.cap) {
                WRITE => s.request.push(value),
                READ => s.at += 1,
                CONTROL if value & 1 != 0 => {
                    s.error = false;
                    s.request.clear();
                }
                CONTROL if value >> 31 != 0 => {
                    let request = core::mem::take(&mut s.request);
                    s.response = (self.respond)(&request);
                    s.at = 0;
                    s.exchanges += 1;
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::testing::Model;
    use super::*;
    use fstart_core::services::timer::ManualTimer;
    use std::vec;

    #[test]
    fn discovery_finds_the_cdat_protocol_and_errors_abort() {
        // Entries: discovery, then CXL table access (0x1E98/2).
        let model = Model::new(0x200, |request: &[u32]| {
            assert_eq!(request[..2], [0x0000_0001, 3]);
            match request[2] {
                0 => vec![0x0000_0001, 3, 0x0100_0001],
                1 => vec![0x0000_0001, 3, 0x0002_1e98],
                _ => vec![0x0000_0001, 3, 0],
            }
        });
        let timer = ManualTimer::new();
        let doe = unsafe { Doe::new(&model, PciAddress::new(0, 1, 0, 0), 0x200) };
        assert_eq!(doe.discover(0, &timer), Ok((1, 0, 1)));
        assert_eq!(doe.supports(0x1e98, 2, &timer), Ok(true));
        assert_eq!(doe.supports(0x1e98, 9, &timer), Ok(false));
        // A mailbox in error is aborted first.
        model.state.borrow_mut().error = true;
        assert_eq!(doe.discover(1, &timer), Ok((0x1e98, 2, 0)));
        assert_eq!(header(0x1e98, 2, 3), [0x0002_1e98, 3]);
        assert_eq!(Error::Timeout.code(), 0x434c_0601);
    }
}
