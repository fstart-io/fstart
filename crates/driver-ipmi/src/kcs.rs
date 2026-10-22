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

const START_WRITE: u8 = 0x61;
const END_WRITE: u8 = 0x62;
const READ_BYTE: u8 = 0x68;

/// 5 s, polled every 10 us.
const TIMEOUT_POLLS: u32 = 500_000;

/// A KCS interface: its data register and its status/command register.
pub struct Kcs<D, P = PioRegister<u8>, S = PioRegister<u8, Status::Register>> {
    data: P,
    status: S,
    delay: D,
}

impl<D: DelayNs> Kcs<D> {
    /// The data port at `base`, status/command `spacing` ports above.
    ///
    /// # Safety
    /// The ports are decoded to the BMC's KCS interface and nothing else
    /// uses it meanwhile.
    pub unsafe fn new(base: u16, spacing: u16, delay: D) -> Self {
        Self::with_registers(PioRegister::new(base), PioRegister::new(base + spacing), delay)
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
        }
    }

    fn wait(
        &mut self,
        done: impl Fn(LocalRegisterCopy<u8, Status::Register>) -> bool,
    ) -> Result<(), Error> {
        for _ in 0..TIMEOUT_POLLS {
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
        self.send(netfn, command, request)?;
        let length = self.receive(buffer)?;
        match buffer[..length] {
            [_, _, 0, ..] => Ok(&buffer[3..length]),
            [_, _, code, ..] => Err(Error::Completion(code)),
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
            bmc.state << 6 | u8::from(bmc.output.is_some())
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
        let end = bmc.writes.iter().position(|&w| w == (true, END_WRITE)).unwrap();
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
        let end = bmc.writes.iter().position(|&w| w == (true, END_WRITE)).unwrap();
        assert_eq!(bmc.writes[end + 1], (false, 0x01));
    }
}
