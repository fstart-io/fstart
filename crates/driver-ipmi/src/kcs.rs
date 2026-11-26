//! The KCS state machine of IPMI 2.0 section 9, polled.
use crate::Error;
use embedded_hal::delay::DelayNs;
use fstart_core::pio::PioRegister;
use fstart_core::services::Timer;
use fstart_core::task;
use tock_registers::{
    LocalRegisterCopy,
    interfaces::{Readable, Writeable},
    register_bitfields,
};

register_bitfields![u8,
    /// The status register; commands are written to the same port.
    pub Status [
        OBF OFFSET(0) NUMBITS(1) [],
        IBF OFFSET(1) NUMBITS(1) [],
        STATE OFFSET(6) NUMBITS(2) [
            Idle = 0,
            Read = 1,
            Write = 2,
            Error = 3
        ]
    ]
];

/// The default KCS data port (IPMI 2.0 appendix C1); status/command at
/// `+ DEFAULT_SPACING`.
pub const DEFAULT_PORT: u16 = 0xca2;
pub const DEFAULT_SPACING: u16 = 1;

/// The default interface as ACPI describes it (`IPI0001` and SPMI), for a
/// BMC implementing IPMI 2.0.
#[cfg(feature = "acpi")]
pub const DEFAULT_ACPI: fstart_acpi::devices::IpmiKcsAcpi = fstart_acpi::devices::IpmiKcsAcpi {
    port: DEFAULT_PORT,
    spacing: DEFAULT_SPACING as u8,
    uid: 0,
    spec_revision: crate::SPEC_REVISION_2_0,
};

const GET_STATUS_ABORT: u8 = 0x60;
const START_WRITE: u8 = 0x61;
const END_WRITE: u8 = 0x62;
const READ_BYTE: u8 = 0x68;

/// Transaction-wide budget, polled every 10 us (at most 5 s of delays).
const TIMEOUT_POLLS: u32 = 500_000;
/// Error exits [`Kcs::abort`] tries before the interface counts as stuck.
const ABORT_ATTEMPTS: u32 = 2;
/// The share of a request's poll budget kept for its abort: one in four
/// polls. A failed request's error exits have at least that, whatever the
/// request used, and never more than the budget leaves.
const ABORT_SHARE: u32 = 4;

/// A KCS interface: its data register and its status/command register.
pub struct Kcs<D, P = PioRegister<u8>, S = PioRegister<u8, Status::Register>> {
    data: P,
    status: S,
    delay: D,
    poll_budget: u32,
    remaining: u32,
    /// The request took WRITE_START: the interface is this caller's to
    /// abort.
    started: bool,
}

impl<D> Kcs<D> {
    /// The data port at `base`, status/command `spacing` ports above.
    ///
    /// # Safety
    /// The ports are decoded to the BMC's KCS interface and nothing else
    /// uses it meanwhile.
    pub unsafe fn new(base: u16, spacing: u16, delay: D) -> Self {
        Self::with_registers(
            PioRegister::new(base),
            PioRegister::new(base + spacing),
            delay,
        )
    }
}

impl<D, P, S> Kcs<D, P, S>
where
    P: Readable<T = u8> + Writeable<T = u8>,
    S: Readable<T = u8, R = Status::Register> + Writeable<T = u8, R = Status::Register>,
{
    /// The interface behind `data` and `status`.
    pub fn with_registers(data: P, status: S, delay: D) -> Self {
        Self {
            data,
            status,
            delay,
            poll_budget: TIMEOUT_POLLS,
            remaining: 0,
            started: false,
        }
    }

    /// Bound the *whole* request, including an endlessly streaming response
    /// and the abort of a failed one: one deadline for both, of which the
    /// abort keeps a share ([`ABORT_SHARE`]). Every readiness check
    /// consumes a poll, even if immediately ready; a failed check
    /// additionally delays 10 us. Intended for latency-sensitive callers
    /// such as SMM, not a fresh timeout for every byte.
    pub fn with_poll_budget(mut self, polls: u32) -> Self {
        self.poll_budget = polls;
        self
    }

    /// No transaction is in progress: idle state, both buffers empty. A
    /// caller that interrupts the interface's owner (SMM) must not start
    /// one otherwise.
    pub fn idle(&self) -> bool {
        let status = self.status.extract();
        status.matches_all(Status::STATE::Idle)
            && !status.is_set(Status::IBF)
            && !status.is_set(Status::OBF)
    }

    /// Drop a stale output byte.
    fn clear_output(&self) {
        if self.status.is_set(Status::OBF) {
            self.data.get();
        }
    }

    fn expect_write_state(&self) -> Result<(), Error> {
        let status = self.status.extract();
        if status.is_set(Status::OBF) && !status.matches_all(Status::STATE::Write) {
            return Err(Error::State(status.get()));
        }
        self.clear_output();
        Ok(())
    }
}

impl<D, P, S> Kcs<D, P, S>
where
    D: DelayNs,
    P: Readable<T = u8> + Writeable<T = u8>,
    S: Readable<T = u8, R = Status::Register> + Writeable<T = u8, R = Status::Register>,
{
    fn wait(
        &mut self,
        done: impl Fn(LocalRegisterCopy<u8, Status::Register>) -> bool,
    ) -> Result<(), Error> {
        while self.remaining != 0 {
            self.remaining -= 1;
            if done(self.status.extract()) {
                return Ok(());
            }
            self.delay.delay_us(10);
        }
        Err(Error::Timeout)
    }

    fn wait_input_empty(&mut self) -> Result<(), Error> {
        self.wait(|status| !status.is_set(Status::IBF))
    }

    fn write_command(&mut self, command: u8) -> Result<(), Error> {
        self.wait_input_empty()?;
        self.clear_output();
        self.status.set(command);
        self.started |= command == START_WRITE;
        self.wait_input_empty()?;
        self.clear_output();
        Ok(())
    }

    fn write_data(&mut self, byte: u8) -> Result<(), Error> {
        self.data.set(byte);
        self.wait_input_empty()?;
        self.expect_write_state()
    }

    fn write_last(&mut self, byte: u8) -> Result<(), Error> {
        self.wait_input_empty()?;
        self.expect_write_state()?;
        self.data.set(byte);
        Ok(())
    }

    fn send(&mut self, netfn: u8, command: u8, request: &[u8]) -> Result<(), Error> {
        self.write_command(START_WRITE)?;
        self.write_data(netfn << 2)?;
        // The command byte and the request; the last byte follows END_WRITE.
        let (&last, body) = request
            .split_last()
            .map_or((&command, &[][..]), |(last, rest)| (last, rest));
        if !request.is_empty() {
            self.write_data(command)?;
        }
        body.iter().try_for_each(|&byte| self.write_data(byte))?;
        self.write_command(END_WRITE)?;
        self.write_last(last)
    }

    /// Read the response into `buffer`; returns its length. Bytes beyond the
    /// buffer are read and dropped.
    ///
    /// Each turn waits until the BMC took the previous READ (IBF clear)
    /// before it reads the state (IPMI 2.0 figure 9-7): read too early, the
    /// state is still READ after the last byte, the BMC's closing dummy
    /// byte is taken for data and a READ in the idle state leaves the
    /// interface in its error state.
    fn receive(&mut self, buffer: &mut [u8]) -> Result<usize, Error> {
        let mut length = 0;
        loop {
            self.wait_input_empty()?;
            let status = self.status.extract();
            match status.read_as_enum(Status::STATE) {
                Some(Status::STATE::Value::Idle) => {
                    // The dummy byte the BMC closes the transfer with.
                    self.clear_output();
                    return Ok(length);
                }
                Some(Status::STATE::Value::Read) => {}
                _ => return Err(Error::State(status.get())),
            }
            self.wait(|status| status.is_set(Status::OBF))?;
            let byte = self.data.get();
            if let Some(slot) = buffer.get_mut(length) {
                *slot = byte;
                length += 1;
            }
            self.data.set(READ_BYTE);
        }
    }

    /// The KCS error exit (IPMI 2.0 §9.15, figure 9-10): GET_STATUS/ABORT,
    /// a zero byte, the BMC's status code read in the read state, READ,
    /// the closing dummy byte in the idle state. Returns the status code.
    fn error_exit(&mut self) -> Result<u8, Error> {
        self.wait_input_empty()?;
        self.status.set(GET_STATUS_ABORT);
        self.wait_input_empty()?;
        self.clear_output();
        self.data.set(0);
        self.wait_input_empty()?;
        let status = self.status.extract();
        if !status.matches_all(Status::STATE::Read) {
            return Err(Error::State(status.get()));
        }
        self.wait(|status| status.is_set(Status::OBF))?;
        let code = self.data.get();
        self.data.set(READ_BYTE);
        self.wait_input_empty()?;
        let status = self.status.extract();
        if !status.matches_all(Status::STATE::Idle) {
            return Err(Error::State(status.get()));
        }
        self.wait(|status| status.is_set(Status::OBF))?;
        self.data.get();
        Ok(code)
    }

    /// Bring the interface back to idle after a failed transaction of this
    /// caller's: the KCS error exit, at most [`ABORT_ATTEMPTS`] of them
    /// within the poll budget together, then the idle state verified
    /// ([`Self::idle`]). Returns the BMC's status code; [`Error::Stuck`]
    /// if the interface is not idle after them, and a caller that shares it
    /// with the OS must stop using it.
    pub fn abort(&mut self) -> Result<u8, Error> {
        self.remaining = self.poll_budget;
        self.error_exits()
    }

    /// [`Self::abort`] within the polls left.
    fn error_exits(&mut self) -> Result<u8, Error> {
        for _ in 0..ABORT_ATTEMPTS {
            if let Ok(code) = self.error_exit()
                && self.idle()
            {
                return Ok(code);
            }
        }
        Err(Error::Stuck)
    }

    /// One request; returns the response data after the NetFn, command and
    /// completion code bytes. `buffer` holds the whole response.
    ///
    /// A transaction that fails once it started (a timeout or a state
    /// error while writing the request, reading the response or taking its
    /// closing byte) is aborted ([`Self::abort`]): the interface is idle
    /// again, or the error is [`Error::Stuck`]. One that could not start
    /// (the interface busy before WRITE_START was written) is not touched.
    /// Request and abort share one poll budget ([`Self::with_poll_budget`]):
    /// the request has all but the abort's share, the abort what is left.
    pub fn request<'a>(
        &mut self,
        netfn: u8,
        command: u8,
        request: &[u8],
        buffer: &'a mut [u8],
    ) -> Result<&'a [u8], Error> {
        let reserve = self.poll_budget.div_ceil(ABORT_SHARE);
        self.remaining = self.poll_budget - reserve;
        self.started = false;
        let length = match self
            .send(netfn, command, request)
            .and_then(|()| self.receive(buffer))
        {
            Ok(length) => length,
            // The interface is this caller's once it took WRITE_START.
            Err(error) if !self.started => return Err(error),
            Err(error) => {
                self.remaining += reserve;
                self.error_exits()?;
                return Err(error);
            }
        };
        // No indexing: this also runs in SMM images, which take no panic
        // formatting code.
        let buffer: &'a [u8] = buffer;
        match buffer.get(..length).ok_or(Error::Short)? {
            [_, _, 0, data @ ..] => Ok(data),
            [_, _, code, ..] => Err(Error::Completion(*code)),
            _ => Err(Error::Short),
        }
    }
}

impl<D, P, S> Kcs<D, P, S>
where
    D: Timer,
    P: Readable<T = u8> + Writeable<T = u8>,
    S: Readable<T = u8, R = Status::Register> + Writeable<T = u8, R = Status::Register>,
{
    async fn wait_async(
        &self,
        start: u64,
        budget_us: u64,
        done: impl Fn(LocalRegisterCopy<u8, Status::Register>) -> bool,
    ) -> Result<(), Error> {
        let elapsed = self.delay.timestamp_us().wrapping_sub(start);
        let remaining = budget_us.saturating_sub(elapsed);
        task::poll_until(&self.delay, remaining, || {
            done(self.status.extract()).then_some(())
        })
        .await
        .ok_or(Error::Timeout)
    }

    async fn wait_input_empty_async(&self, start: u64, budget_us: u64) -> Result<(), Error> {
        self.wait_async(start, budget_us, |status| !status.is_set(Status::IBF))
            .await
    }

    async fn write_command_async(
        &mut self,
        command: u8,
        start: u64,
        budget_us: u64,
    ) -> Result<(), Error> {
        self.wait_input_empty_async(start, budget_us).await?;
        self.clear_output();
        self.status.set(command);
        self.started |= command == START_WRITE;
        self.wait_input_empty_async(start, budget_us).await?;
        self.clear_output();
        Ok(())
    }

    async fn write_data_async(
        &mut self,
        byte: u8,
        start: u64,
        budget_us: u64,
    ) -> Result<(), Error> {
        self.data.set(byte);
        self.wait_input_empty_async(start, budget_us).await?;
        self.expect_write_state()
    }

    async fn send_async(
        &mut self,
        netfn: u8,
        command: u8,
        request: &[u8],
        start: u64,
        budget_us: u64,
    ) -> Result<(), Error> {
        self.write_command_async(START_WRITE, start, budget_us)
            .await?;
        self.write_data_async(netfn << 2, start, budget_us).await?;
        let (&last, body) = request
            .split_last()
            .map_or((&command, &[][..]), |(last, rest)| (last, rest));
        if !request.is_empty() {
            self.write_data_async(command, start, budget_us).await?;
        }
        for &byte in body {
            self.write_data_async(byte, start, budget_us).await?;
        }
        self.write_command_async(END_WRITE, start, budget_us)
            .await?;
        self.wait_input_empty_async(start, budget_us).await?;
        self.expect_write_state()?;
        self.data.set(last);
        Ok(())
    }

    async fn receive_async(
        &mut self,
        buffer: &mut [u8],
        start: u64,
        budget_us: u64,
    ) -> Result<usize, Error> {
        let mut length = 0;
        loop {
            self.wait_input_empty_async(start, budget_us).await?;
            let status = self.status.extract();
            match status.read_as_enum(Status::STATE) {
                Some(Status::STATE::Value::Idle) => {
                    self.clear_output();
                    return Ok(length);
                }
                Some(Status::STATE::Value::Read) => {}
                _ => return Err(Error::State(status.get())),
            }
            self.wait_async(start, budget_us, |status| status.is_set(Status::OBF))
                .await?;
            let byte = self.data.get();
            if let Some(slot) = buffer.get_mut(length) {
                *slot = byte;
                length += 1;
            }
            self.data.set(READ_BYTE);
        }
    }

    async fn error_exit_async(&mut self, start: u64, budget_us: u64) -> Result<u8, Error> {
        self.wait_input_empty_async(start, budget_us).await?;
        self.status.set(GET_STATUS_ABORT);
        self.wait_input_empty_async(start, budget_us).await?;
        self.clear_output();
        self.data.set(0);
        self.wait_input_empty_async(start, budget_us).await?;
        let status = self.status.extract();
        if !status.matches_all(Status::STATE::Read) {
            return Err(Error::State(status.get()));
        }
        self.wait_async(start, budget_us, |status| status.is_set(Status::OBF))
            .await?;
        let code = self.data.get();
        self.data.set(READ_BYTE);
        self.wait_input_empty_async(start, budget_us).await?;
        let status = self.status.extract();
        if !status.matches_all(Status::STATE::Idle) {
            return Err(Error::State(status.get()));
        }
        self.wait_async(start, budget_us, |status| status.is_set(Status::OBF))
            .await?;
        self.data.get();
        Ok(code)
    }

    async fn error_exits_async(&mut self, start: u64, budget_us: u64) -> Result<u8, Error> {
        for _ in 0..ABORT_ATTEMPTS {
            if let Ok(code) = self.error_exit_async(start, budget_us).await
                && self.idle()
            {
                return Ok(code);
            }
        }
        Err(Error::Stuck)
    }

    /// Asynchronous counterpart to [`Self::abort`], polling via
    /// [`task::poll_until`] with a monotonic deadline so sibling tasks can run.
    pub async fn abort_async(&mut self) -> Result<u8, Error> {
        let start = self.delay.timestamp_us();
        let budget_us = u64::from(self.poll_budget) * 10;
        self.error_exits_async(start, budget_us).await
    }

    /// Asynchronous counterpart to [`Self::request`], polling via
    /// [`task::poll_until`] with a monotonic deadline so sibling tasks (e.g.
    /// SMU DXIO waits or [`task::join`]) can overlap with BMC KCS latency.
    ///
    /// Kept separate from the synchronous [`Self::request`] path so SMM
    /// images (which enforce link-time panic absence) link only the
    /// synchronous implementation.
    pub async fn request_async<'a>(
        &mut self,
        netfn: u8,
        command: u8,
        request: &[u8],
        buffer: &'a mut [u8],
    ) -> Result<&'a [u8], Error> {
        let start = self.delay.timestamp_us();
        let total_us = u64::from(self.poll_budget) * 10;
        let reserve_us = total_us.div_ceil(u64::from(ABORT_SHARE));
        let request_budget = total_us - reserve_us;
        self.started = false;
        let result = match self
            .send_async(netfn, command, request, start, request_budget)
            .await
        {
            Ok(()) => self.receive_async(buffer, start, request_budget).await,
            Err(error) => Err(error),
        };
        let length = match result {
            Ok(length) => length,
            Err(error) if !self.started => return Err(error),
            Err(error) => {
                self.error_exits_async(start, total_us).await?;
                return Err(error);
            }
        };
        let buffer: &'a [u8] = buffer;
        match buffer.get(..length).ok_or(Error::Short)? {
            [_, _, 0, data @ ..] => Ok(data),
            [_, _, code, ..] => Err(Error::Completion(*code)),
            _ => Err(Error::Short),
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use core::cell::RefCell;
    use std::{collections::VecDeque, vec, vec::Vec};

    /// A BMC: collects the request, then answers with `response`.
    #[derive(Default)]
    struct Bmc {
        state: u8,
        request: Vec<u8>,
        last: bool,
        response: VecDeque<u8>,
        answer: Vec<u8>,
        output: Option<u8>,
        busy: bool,
        /// A READ the BMC has not taken yet: IBF is set until the next
        /// status read, which takes it (the next byte, or the end: idle
        /// and a dummy byte).
        pending_read: bool,
        /// (command port, value) of every write, in order.
        writes: Vec<(bool, u8)>,
        /// Failures: the error state after this many request bytes (the
        /// write phase), no byte after the first READ (the read phase),
        /// the error state instead of idle at the end (the closing byte).
        fail_after: Option<usize>,
        stall_read: bool,
        end_in_error: bool,
        /// GET_STATUS/ABORT is ignored: the interface stays where it is.
        deaf: bool,
        /// The error exit's phase: 1 after GET_STATUS/ABORT.
        aborting: u8,
    }
    const IDLE: u8 = 0;
    const READ: u8 = 1;
    const WRITE: u8 = 2;
    const ERROR: u8 = 3;

    struct Data<'a>(&'a RefCell<Bmc>);
    struct Command<'a>(&'a RefCell<Bmc>);
    impl Readable for Data<'_> {
        type T = u8;
        type R = ();
        fn get(&self) -> u8 {
            self.0.borrow_mut().output.take().unwrap_or(0)
        }
    }
    impl Writeable for Data<'_> {
        type T = u8;
        type R = ();
        fn set(&self, value: u8) {
            let mut bmc = self.0.borrow_mut();
            bmc.writes.push((false, value));
            if bmc.aborting == 1 && bmc.state == WRITE {
                // The error exit's zero byte: the status code to read.
                assert_eq!(value, 0);
                bmc.aborting = 0;
                bmc.state = READ;
                bmc.response.clear();
                bmc.output = Some(0x83);
                return;
            }
            if bmc.state == WRITE {
                if bmc.fail_after == Some(bmc.request.len()) {
                    bmc.state = ERROR;
                    bmc.output = Some(0xff);
                    return;
                }
                bmc.request.push(value);
                if bmc.last {
                    bmc.last = false;
                    bmc.state = READ;
                    bmc.response = bmc.answer.iter().copied().collect();
                    bmc.output = bmc.response.pop_front();
                }
            } else if value == READ_BYTE {
                if bmc.state == READ {
                    bmc.pending_read = true;
                } else {
                    bmc.state = ERROR;
                }
            }
        }
    }
    impl Readable for Command<'_> {
        type T = u8;
        type R = Status::Register;
        fn get(&self) -> u8 {
            let mut bmc = self.0.borrow_mut();
            if bmc.pending_read {
                bmc.pending_read = false;
                match bmc.response.pop_front() {
                    _ if bmc.stall_read => bmc.stall_read = false,
                    Some(byte) => bmc.output = Some(byte),
                    None if bmc.end_in_error => {
                        bmc.end_in_error = false;
                        bmc.state = ERROR;
                    }
                    None => {
                        bmc.state = IDLE;
                        bmc.output = Some(0);
                    }
                }
                // This read still sees the READ being taken.
                return READ << 6 | 1 << 1;
            }
            bmc.state << 6 | u8::from(bmc.output.is_some()) | (u8::from(bmc.busy) << 1)
        }
    }
    impl Writeable for Command<'_> {
        type T = u8;
        type R = Status::Register;
        fn set(&self, value: u8) {
            let mut bmc = self.0.borrow_mut();
            bmc.writes.push((true, value));
            match value {
                START_WRITE => {
                    bmc.state = WRITE;
                    bmc.request.clear();
                    bmc.output = Some(0);
                }
                END_WRITE => bmc.last = true,
                GET_STATUS_ABORT if bmc.deaf => {}
                GET_STATUS_ABORT => {
                    bmc.aborting = 1;
                    bmc.state = WRITE;
                    bmc.last = false;
                    bmc.pending_read = false;
                    bmc.output = Some(0);
                }
                _ => panic!("unexpected command {value:#x}"),
            }
        }
    }

    struct NoDelay;
    impl DelayNs for NoDelay {
        fn delay_ns(&mut self, _: u32) {}
    }

    fn kcs(bmc: &RefCell<Bmc>) -> Kcs<NoDelay, Data<'_>, Command<'_>> {
        Kcs::with_registers(Data(bmc), Command(bmc), NoDelay)
    }

    #[test]
    fn request_framing_puts_the_last_byte_after_end_write() {
        let bmc = RefCell::new(Bmc {
            answer: vec![0x2c << 2, 0x20, 0, 0xaa, 0xbb],
            ..Bmc::default()
        });
        let mut buffer = [0; 8];
        let data = kcs(&bmc)
            .request(0x2e, 0x20, &[1, 2, 3], &mut buffer)
            .unwrap();
        assert_eq!(data, [0xaa, 0xbb]);
        let bmc = bmc.into_inner();
        assert_eq!(bmc.request, [0x2e << 2, 0x20, 1, 2, 3]);
        let commands: Vec<_> = bmc.writes.iter().filter(|(command, _)| *command).collect();
        assert_eq!(commands, [&(true, START_WRITE), &(true, END_WRITE)]);
        let end = bmc
            .writes
            .iter()
            .position(|&w| w == (true, END_WRITE))
            .unwrap();
        assert_eq!(bmc.writes[end + 1], (false, 3));
        // Every response byte but the last is acknowledged with READ_BYTE.
        assert_eq!(bmc.writes[end + 2..], [(false, READ_BYTE); 5]);
    }

    #[test]
    fn request_without_data_ends_with_the_command_and_reports_completion_codes() {
        let bmc = RefCell::new(Bmc {
            answer: vec![0x04, 0x01, 0xc1],
            ..Bmc::default()
        });
        let mut buffer = [0; 8];
        assert_eq!(
            kcs(&bmc).request(0x00, 0x01, &[], &mut buffer),
            Err(Error::Completion(0xc1))
        );
        let bmc = bmc.into_inner();
        assert_eq!(bmc.request, [0x00, 0x01]);
        let end = bmc
            .writes
            .iter()
            .position(|&w| w == (true, END_WRITE))
            .unwrap();
        assert_eq!(bmc.writes[end + 1], (false, 0x01));
    }

    #[test]
    fn transaction_budget_bounds_busy_and_progressing_bmcs() {
        let busy = RefCell::new(Bmc {
            busy: true,
            ..Bmc::default()
        });
        let mut buffer = [0; 8];
        assert_eq!(
            kcs(&busy)
                .with_poll_budget(4)
                .request(0, 1, &[], &mut buffer),
            Err(Error::Timeout)
        );
        assert!(busy.borrow().writes.is_empty());
        // Readiness always succeeds, but the response exceeds both the
        // buffer and the budget. Progress must not reset the timeout.
        let streaming = RefCell::new(Bmc {
            answer: vec![0; 1024],
            ..Bmc::default()
        });
        assert_eq!(
            kcs(&streaming)
                .with_poll_budget(32)
                .request(0, 1, &[], &mut buffer),
            Err(Error::Timeout)
        );
        // The transaction's writes within its budget, then the error exit
        // (GET_STATUS/ABORT, the zero byte, READ), which leaves it idle.
        let streaming = streaming.into_inner();
        let abort = streaming
            .writes
            .iter()
            .position(|&w| w == (true, GET_STATUS_ABORT))
            .unwrap();
        assert!(abort <= 32);
        assert_eq!(
            streaming.writes[abort..],
            [(true, GET_STATUS_ABORT), (false, 0), (false, READ_BYTE)]
        );
        assert_eq!((streaming.state, streaming.output), (IDLE, None));
    }

    /// A transaction that fails in any phase is aborted, and the interface
    /// is idle for the OS again; one whose BMC ignores the abort is stuck.
    #[test]
    fn a_failed_transaction_leaves_the_interface_idle_or_says_it_is_stuck() {
        let answer = vec![0x05 << 2, 0x02, 0, 1, 2, 3];
        let cases = [
            (
                "write",
                Bmc {
                    fail_after: Some(2),
                    ..Bmc::default()
                },
            ),
            (
                "read",
                Bmc {
                    stall_read: true,
                    ..Bmc::default()
                },
            ),
            (
                "closing byte",
                Bmc {
                    end_in_error: true,
                    ..Bmc::default()
                },
            ),
        ];
        for (phase, bmc) in cases {
            let bmc = RefCell::new(Bmc {
                answer: answer.clone(),
                ..bmc
            });
            let mut buffer = [0; 8];
            let mut kcs = kcs(&bmc).with_poll_budget(64);
            let result = kcs.request(0x04, 0x02, &[1, 2, 3], &mut buffer);
            assert!(
                matches!(result, Err(Error::Timeout | Error::State(_))),
                "{phase}: {result:?}"
            );
            assert!(kcs.idle(), "{phase}: idle again");
            let bmc = bmc.into_inner();
            assert_eq!((bmc.state, bmc.output), (IDLE, None), "{phase}");
            assert!(bmc.writes.contains(&(true, GET_STATUS_ABORT)), "{phase}");
        }
        let deaf = RefCell::new(Bmc {
            answer,
            fail_after: Some(1),
            deaf: true,
            ..Bmc::default()
        });
        let mut buffer = [0; 8];
        assert_eq!(
            kcs(&deaf)
                .with_poll_budget(64)
                .request(0x04, 0x02, &[1], &mut buffer),
            Err(Error::Stuck)
        );
        // Two error exits tried, no more.
        let aborts = deaf
            .into_inner()
            .writes
            .iter()
            .filter(|&&w| w == (true, GET_STATUS_ABORT))
            .count();
        assert_eq!(aborts, 2);
    }

    /// The delays of a request, counted.
    struct Counted<'a>(&'a core::cell::Cell<u32>);
    impl DelayNs for Counted<'_> {
        fn delay_ns(&mut self, _: u32) {
            self.0.set(self.0.get() + 1);
        }
    }

    /// One deadline for a request and its cleanup: a BMC that stalls the
    /// request and ignores its aborts costs the poll budget once, not a
    /// budget for the request and another for each abort; the aborts keep
    /// their share of it. A request whose WRITE_START never went out (the
    /// interface busy) is not aborted: the interface is not this caller's.
    #[test]
    fn a_request_and_its_abort_share_one_deadline() {
        let delays = core::cell::Cell::new(0);
        let deaf = RefCell::new(Bmc {
            answer: vec![0x05 << 2, 0x02, 0],
            stall_read: true,
            deaf: true,
            ..Bmc::default()
        });
        let mut buffer = [0; 8];
        let mut counted =
            Kcs::with_registers(Data(&deaf), Command(&deaf), Counted(&delays)).with_poll_budget(40);
        assert_eq!(
            counted.request(0x04, 0x02, &[1], &mut buffer),
            Err(Error::Stuck)
        );
        assert!(delays.get() <= 40, "{} delays", delays.get());
        let deaf = deaf.into_inner();
        let aborts = deaf
            .writes
            .iter()
            .filter(|&&w| w == (true, GET_STATUS_ABORT))
            .count();
        assert!(aborts >= 1, "the abort's share was left for it");

        let busy = RefCell::new(Bmc {
            busy: true,
            ..Bmc::default()
        });
        let mut kcs = kcs(&busy).with_poll_budget(40);
        assert_eq!(kcs.request(0, 1, &[], &mut buffer), Err(Error::Timeout));
        assert!(
            busy.into_inner().writes.is_empty(),
            "no WRITE_START, no abort"
        );
    }

    #[test]
    fn oem_sel_request_and_returned_record_id() {
        let bmc = RefCell::new(Bmc {
            answer: vec![0x2c, 0x44, 0, 0x34, 0x12],
            ..Bmc::default()
        });
        let record = crate::OemSelRecord::new([0x5a; 13]);
        assert_eq!(kcs(&bmc).add_oem_sel(&record), Ok(0x1234));
        let bmc = bmc.into_inner();
        assert_eq!(&bmc.request[..5], &[0x28, 0x44, 0, 0, 0xe0]);
        assert_eq!(&bmc.request[5..], &[0x5a; 13]);
    }

    #[test]
    fn the_read_phase_ends_idle_without_taking_the_closing_byte_for_data() {
        let bmc = RefCell::new(Bmc {
            answer: vec![0x07 << 2, 0x01, 0, 0x20],
            ..Bmc::default()
        });
        let mut buffer = [0; 8];
        assert_eq!(
            kcs(&bmc).request(crate::NETFN_APP, 0x01, &[], &mut buffer),
            Ok(&[0x20][..])
        );
        let bmc = bmc.into_inner();
        assert_eq!((bmc.state, bmc.output), (IDLE, None));
    }

    #[test]
    fn a_platform_event_carries_the_generator_and_the_event() {
        let bmc = RefCell::new(Bmc {
            answer: vec![0x05 << 2, 0x02, 0],
            ..Bmc::default()
        });
        let record = crate::SelRecord {
            record_id: zerocopy::little_endian::U16::ZERO,
            record_type: 2,
            timestamp: zerocopy::little_endian::U32::ZERO,
            generator_id: zerocopy::little_endian::U16::new(0x21),
            event_message_revision: 4,
            sensor_type: 0x0c,
            sensor_number: 0xff,
            event_dir_type: 0x6f,
            event_data: [0xa5, 3, 21],
        };
        kcs(&bmc).platform_event(&record).unwrap();
        assert_eq!(
            bmc.into_inner().request,
            [0x04 << 2, 0x02, 0x21, 4, 0x0c, 0xff, 0x6f, 0xa5, 3, 21]
        );
    }

    #[test]
    fn set_acpi_power_state_sets_both_fields() {
        let bmc = RefCell::new(Bmc {
            answer: vec![0x07 << 2, 0x06, 0],
            ..Bmc::default()
        });
        let (system, device) = crate::sleep_power_states(5).unwrap();
        kcs(&bmc).set_acpi_power_state(system, device).unwrap();
        assert_eq!(bmc.into_inner().request, [0x06 << 2, 0x06, 0x85, 0x83]);
        assert_eq!(crate::sleep_power_states(0), Some((0, 0)));
        assert_eq!(crate::sleep_power_states(2), None);
    }

    #[test]
    fn sleep_notify_only_on_an_idle_interface() {
        let notify = crate::SleepNotify {
            kcs_port: DEFAULT_PORT,
            kcs_spacing: 1,
            sw_smi_base: 0xc0,
        };
        let bmc = || {
            RefCell::new(Bmc {
                answer: vec![0x07 << 2, 0x06, 0],
                ..Bmc::default()
            })
        };
        // Not a notification.
        let idle = bmc();
        assert_eq!(notify.report_on(kcs(&idle), 0xe1), None);
        assert_eq!(notify.report_on(kcs(&idle), 0xc2), None);
        // S5.
        assert_eq!(notify.report_on(kcs(&idle), 0xc5), Some(Ok(())));
        assert_eq!(idle.into_inner().request, [0x06 << 2, 0x06, 0x85, 0x83]);
        // The OS's transaction is in progress: nothing is written.
        let busy = bmc();
        busy.borrow_mut().state = READ;
        assert_eq!(notify.report_on(kcs(&busy), 0xc5), None);
        let busy = bmc();
        busy.borrow_mut().busy = true;
        assert_eq!(notify.report_on(kcs(&busy), 0xc5), None);
        assert!(busy.into_inner().writes.is_empty());
    }

    #[test]
    fn async_request_matches_blocking_wire_format_and_overlaps_via_join() {
        let blocking_bmc = RefCell::new(Bmc {
            answer: vec![0x2c << 2, 0x20, 0, 0xaa, 0xbb],
            ..Bmc::default()
        });
        let async_bmc = RefCell::new(Bmc {
            answer: vec![0x2c << 2, 0x20, 0, 0xaa, 0xbb],
            ..Bmc::default()
        });
        let mut sync_buf = [0; 8];
        let expected = kcs(&blocking_bmc)
            .request(0x2e, 0x20, &[1, 2, 3], &mut sync_buf)
            .unwrap()
            .to_vec();

        let timer = fstart_core::services::ManualTimer::ticking();
        let mut async_kcs = Kcs::with_registers(Data(&async_bmc), Command(&async_bmc), &timer);
        let mut async_buf = [0; 8];
        let (kcs_res, sibling_done) = task::block_on(task::join(
            async {
                async_kcs
                    .request_async(0x2e, 0x20, &[1, 2, 3], &mut async_buf)
                    .await
                    .map(|d| d.to_vec())
            },
            async {
                task::delay_us(&timer, 25).await;
                true
            },
        ));
        assert!(sibling_done);
        assert_eq!(kcs_res.unwrap(), expected);

        let blocking_bmc = blocking_bmc.into_inner();
        let async_bmc = async_bmc.into_inner();
        assert_eq!(async_bmc.request, blocking_bmc.request);
        assert_eq!(async_bmc.writes, blocking_bmc.writes);
        assert_eq!((async_bmc.state, async_bmc.output), (IDLE, None));
    }

    #[test]
    fn async_request_handles_completion_error_abort_and_stuck() {
        let timer = fstart_core::services::ManualTimer::ticking();
        let bmc = RefCell::new(Bmc {
            answer: vec![0x04, 0x01, 0xc1],
            ..Bmc::default()
        });
        let mut buf = [0; 8];
        let mut client = Kcs::with_registers(Data(&bmc), Command(&bmc), &timer);
        assert_eq!(
            task::block_on(client.request_async(0x00, 0x01, &[], &mut buf)),
            Err(Error::Completion(0xc1))
        );
        assert_eq!(bmc.into_inner().request, [0x00, 0x01]);

        let failing = RefCell::new(Bmc {
            answer: vec![0x05 << 2, 0x02, 0, 1, 2],
            fail_after: Some(2),
            ..Bmc::default()
        });
        let mut client =
            Kcs::with_registers(Data(&failing), Command(&failing), &timer).with_poll_budget(64);
        assert!(matches!(
            task::block_on(client.request_async(0x04, 0x02, &[1, 2, 3], &mut buf)),
            Err(Error::Timeout | Error::State(_))
        ));
        assert!(client.idle());
        let failing = failing.into_inner();
        assert_eq!((failing.state, failing.output), (IDLE, None));
        assert!(failing.writes.contains(&(true, GET_STATUS_ABORT)));

        let deaf = RefCell::new(Bmc {
            answer: vec![0x05 << 2, 0x02, 0],
            fail_after: Some(1),
            deaf: true,
            ..Bmc::default()
        });
        let mut client =
            Kcs::with_registers(Data(&deaf), Command(&deaf), &timer).with_poll_budget(64);
        assert_eq!(
            task::block_on(client.request_async(0x04, 0x02, &[1], &mut buf)),
            Err(Error::Stuck)
        );
    }
}
