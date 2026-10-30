//! The KCS state machine of IPMI 2.0 section 9, polled.
use crate::Error;
use embedded_hal::delay::DelayNs;
use fstart_core::pio::PioRegister;
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
        STATE OFFSET(6) NUMBITS(2) [Idle = 0, Read = 1, Write = 2, Error = 3]
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

const START_WRITE: u8 = 0x61;
const END_WRITE: u8 = 0x62;
const READ_BYTE: u8 = 0x68;

/// Transaction-wide budget, polled every 10 us (at most 5 s of delays).
const TIMEOUT_POLLS: u32 = 500_000;

/// A KCS interface: its data register and its status/command register.
pub struct Kcs<D, P = PioRegister<u8>, S = PioRegister<u8, Status::Register>> {
    data: P,
    status: S,
    delay: D,
    poll_budget: u32,
    remaining: u32,
}

impl<D: DelayNs> Kcs<D> {
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
    D: DelayNs,
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
        }
    }

    /// Bound the *whole* request, including an endlessly streaming response.
    /// Every readiness check consumes a poll, even if immediately ready; a
    /// failed check additionally delays 10 us. Intended for latency-sensitive
    /// callers such as SMM, not a fresh timeout for every byte.
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

    /// Drop a stale output byte.
    fn clear_output(&self) {
        if self.status.is_set(Status::OBF) {
            self.data.get();
        }
    }

    fn write_command(&mut self, command: u8) -> Result<(), Error> {
        self.wait_input_empty()?;
        self.clear_output();
        self.status.set(command);
        self.wait_input_empty()?;
        self.clear_output();
        Ok(())
    }

    fn expect_write_state(&self) -> Result<(), Error> {
        let status = self.status.extract();
        if status.is_set(Status::OBF) && !status.matches_all(Status::STATE::Write) {
            return Err(Error::State(status.get()));
        }
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
    fn receive(&mut self, buffer: &mut [u8]) -> Result<usize, Error> {
        self.wait_input_empty()?;
        let mut length = 0;
        loop {
            let status = self.status.extract();
            match status.read_as_enum(Status::STATE) {
                Some(Status::STATE::Value::Idle) => return Ok(length),
                Some(Status::STATE::Value::Read) => {}
                _ => return Err(Error::State(status.get())),
            }
            self.wait(|status| status.is_set(Status::OBF))?;
            let byte = self.data.get();
            if let Some(slot) = buffer.get_mut(length) {
                *slot = byte;
                length += 1;
            }
            self.wait_input_empty()?;
            self.data.set(READ_BYTE);
        }
    }

    /// One request; returns the response data after the NetFn, command and
    /// completion code bytes. `buffer` holds the whole response.
    pub fn request<'a>(
        &mut self,
        netfn: u8,
        command: u8,
        request: &[u8],
        buffer: &'a mut [u8],
    ) -> Result<&'a [u8], Error> {
        self.remaining = self.poll_budget;
        self.send(netfn, command, request)?;
        let length = self.receive(buffer)?;
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
        /// (command port, value) of every write, in order.
        writes: Vec<(bool, u8)>,
    }
    const IDLE: u8 = 0;
    const READ: u8 = 1;
    const WRITE: u8 = 2;

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
            if bmc.state == WRITE {
                bmc.request.push(value);
                if bmc.last {
                    bmc.last = false;
                    bmc.state = READ;
                    bmc.response = bmc.answer.iter().copied().collect();
                    bmc.output = bmc.response.pop_front();
                }
            } else if value == READ_BYTE {
                match bmc.response.pop_front() {
                    Some(byte) => bmc.output = Some(byte),
                    None => bmc.state = IDLE,
                }
            }
        }
    }
    impl Readable for Command<'_> {
        type T = u8;
        type R = Status::Register;
        fn get(&self) -> u8 {
            let bmc = self.0.borrow();
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
        assert!(streaming.borrow().writes.len() <= 32);
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
}
