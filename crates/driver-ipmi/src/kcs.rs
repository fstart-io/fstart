//! The KCS state machine of IPMI 2.0 section 9, polled.
use crate::Error;
use embedded_hal::delay::DelayNs;
use fstart_core::pio::{inb, outb};
use tock_registers::{LocalRegisterCopy, register_bitfields};

register_bitfields![u8,
    Status [
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

pub struct Kcs<D> {
    data: u16,
    command: u16,
    delay: D,
}

impl<D: DelayNs> Kcs<D> {
    /// The data port at `base`, status/command `spacing` ports above.
    ///
    /// # Safety
    /// The ports are decoded to the BMC's KCS interface and nothing else
    /// uses it meanwhile.
    pub unsafe fn new(base: u16, spacing: u16, delay: D) -> Self {
        Self {
            data: base,
            command: base + spacing,
            delay,
        }
    }

    fn status(&self) -> LocalRegisterCopy<u8, Status::Register> {
        // SAFETY: the constructor's contract.
        LocalRegisterCopy::new(unsafe { inb(self.command) })
    }

    fn wait(
        &mut self,
        done: impl Fn(LocalRegisterCopy<u8, Status::Register>) -> bool,
    ) -> Result<(), Error> {
        for _ in 0..TIMEOUT_POLLS {
            if done(self.status()) {
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
        if self.status().is_set(Status::OBF) {
            // SAFETY: the constructor's contract.
            unsafe { inb(self.data) };
        }
    }

    fn write_command(&mut self, command: u8) -> Result<(), Error> {
        self.wait_input_empty()?;
        self.clear_output();
        // SAFETY: the constructor's contract.
        unsafe { outb(self.command, command) };
        self.wait_input_empty()?;
        self.clear_output();
        Ok(())
    }

    fn expect_write_state(&self) -> Result<(), Error> {
        let status = self.status();
        if status.is_set(Status::OBF) && !status.matches_all(Status::STATE::Write) {
            return Err(Error::State(status.get()));
        }
        self.clear_output();
        Ok(())
    }

    fn write_data(&mut self, byte: u8) -> Result<(), Error> {
        // SAFETY: the constructor's contract.
        unsafe { outb(self.data, byte) };
        self.wait_input_empty()?;
        self.expect_write_state()
    }

    fn write_last(&mut self, byte: u8) -> Result<(), Error> {
        self.wait_input_empty()?;
        self.expect_write_state()?;
        // SAFETY: the constructor's contract.
        unsafe { outb(self.data, byte) };
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
            let status = self.status();
            match status.read_as_enum(Status::STATE) {
                Some(Status::STATE::Value::Idle) => return Ok(length),
                Some(Status::STATE::Value::Read) => {}
                _ => return Err(Error::State(status.get())),
            }
            self.wait(|status| status.is_set(Status::OBF))?;
            // SAFETY: the constructor's contract.
            let byte = unsafe { inb(self.data) };
            if let Some(slot) = buffer.get_mut(length) {
                *slot = byte;
                length += 1;
            }
            self.wait_input_empty()?;
            // SAFETY: the constructor's contract.
            unsafe { outb(self.data, READ_BYTE) };
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
