// SPDX-License-Identifier: MIT OR Apache-2.0
//! ChromeEC host protocol v3 over linear LPC/eSPI I/O.
//! This backend is not MEC EMI or RTK; those need distinct access mechanisms.
#![no_std]
use zerocopy::{
    FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned,
    byteorder::{LittleEndian, U16, U32},
};
const PACKET: u16 = 0x800;
const STATUS: u16 = 0x204;
const DATA: u16 = 0x200;
const CAPACITY: usize = 256;
pub trait Io {
    fn read8(&mut self, port: u16) -> u8;
    fn write8(&mut self, port: u16, value: u8);
}
pub trait Clock {
    fn now_us(&mut self) -> u64;
    fn delay_us(&mut self, us: u32);
}
#[repr(C)]
#[derive(Clone, Copy, FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned)]
struct Request {
    version: u8,
    checksum: u8,
    command: U16<LittleEndian>,
    command_version: u8,
    reserved: u8,
    length: U16<LittleEndian>,
}
#[repr(C)]
#[derive(Clone, Copy, FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned)]
struct Response {
    version: u8,
    checksum: u8,
    result: U16<LittleEndian>,
    length: U16<LittleEndian>,
    reserved: U16<LittleEndian>,
}
#[repr(C)]
#[derive(Clone, Copy, FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned)]
struct CbiRequest {
    tag: U32<LittleEndian>,
    flags: U32<LittleEndian>,
}
const _: () = assert!(
    size_of::<Request>() == 8 && size_of::<Response>() == 8 && size_of::<CbiRequest>() == 8
);
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Unavailable,
    UnsupportedProtocol,
    Length,
    Timeout,
    Poisoned,
    Header,
    Checksum,
    Rejected(u16),
}
fn checksum(bytes: &[u8]) -> u8 {
    bytes.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte))
}
fn wait(io: &mut impl Io, clock: &mut impl Clock) -> Result<(), Error> {
    let start = clock.now_us();
    for _ in 0..=300000 {
        let status = io.read8(STATUS);
        if status == 0xff {
            return Err(Error::Unavailable);
        }
        if status & 6 == 0 {
            return Ok(());
        }
        if clock.now_us().wrapping_sub(start) >= 30_000_000 {
            break;
        }
        clock.delay_us(100);
    }
    Err(Error::Timeout)
}
/// Exclusive serialized host-command owner. A submitted timeout/malformed
/// response poisons this owner: no duplicate mutation or silent protocol retry.
pub struct Ec<I> {
    io: I,
    poisoned: bool,
}
impl<I: Io> Ec<I> {
    /// Discover native protocol v3 through the linear EC memory map. `io` must
    /// own the decoded linear LPC/eSPI windows, NOT a MEC/RTK indirect backend.
    pub fn discover(mut io: I) -> Result<Self, Error> {
        if io.read8(0x920) != b'E' || io.read8(0x921) != b'C' {
            return Err(Error::Unavailable);
        }
        let flags = io.read8(0x927);
        if flags == 0xff {
            return Err(Error::Unavailable);
        }
        if flags & 2 == 0 {
            return Err(Error::UnsupportedProtocol);
        }
        Ok(Self {
            io,
            poisoned: false,
        })
    }
    pub fn command(
        &mut self,
        command: u16,
        version: u8,
        input: &[u8],
        output: &mut [u8],
        clock: &mut impl Clock,
    ) -> Result<usize, Error> {
        if self.poisoned {
            return Err(Error::Poisoned);
        }
        if input.len() > CAPACITY - 8 || output.len() > CAPACITY - 8 {
            return Err(Error::Length);
        }
        wait(&mut self.io, clock)?;
        let mut request = Request {
            version: 3,
            checksum: 0,
            command: U16::new(command),
            command_version: version,
            reserved: 0,
            length: U16::new(input.len() as u16),
        };
        request.checksum = checksum(request.as_bytes())
            .wrapping_add(checksum(input))
            .wrapping_neg();
        // Payload before header, command trigger last, as native LPC requires.
        for (i, &byte) in input.iter().enumerate() {
            self.io.write8(PACKET + 8 + i as u16, byte);
        }
        for (i, &byte) in request.as_bytes().iter().enumerate() {
            self.io.write8(PACKET + i as u16, byte);
        }
        self.poisoned = true;
        self.io.write8(STATUS, 0xda);
        wait(&mut self.io, clock)?;
        let result = self.io.read8(DATA);
        if result == 0xff {
            return Err(Error::Unavailable);
        }
        if result != 0 {
            // IN_PROGRESS (8) can outlive the transport's busy bit. Do not
            // overwrite its request or silently run RESEND_RESPONSE.
            if result != 8 {
                self.poisoned = false;
            }
            return Err(Error::Rejected(u16::from(result)));
        }
        let mut bytes = [0u8; CAPACITY];
        for (i, byte) in bytes[..8].iter_mut().enumerate() {
            *byte = self.io.read8(PACKET + i as u16);
        }
        let response = Response::read_from_bytes(&bytes[..8]).map_err(|_| Error::Header)?;
        if response.version != 3 || response.reserved.get() != 0 {
            return Err(Error::Header);
        }
        let length = usize::from(response.length.get());
        if length > output.len() || length > CAPACITY - 8 {
            return Err(Error::Length);
        }
        for (i, byte) in bytes[8..8 + length].iter_mut().enumerate() {
            *byte = self.io.read8(PACKET + 8 + i as u16);
        }
        if checksum(&bytes[..8 + length]) != 0 {
            return Err(Error::Checksum);
        }
        let result = response.result.get();
        if result == 8 {
            return Err(Error::Rejected(result));
        }
        self.poisoned = false;
        if result != 0 {
            return Err(Error::Rejected(result));
        }
        // Publish caller data only after complete framing/checksum validation.
        output[..length].copy_from_slice(&bytes[8..8 + length]);
        Ok(length)
    }
    /// Legacy GET_BOARD_VERSION is command 6 with a two-byte response, NOT
    /// CBI tag0. Firmware can report different values through the two paths.
    pub fn board_version(&mut self, clock: &mut impl Clock) -> Result<u16, Error> {
        let mut data = [0; 2];
        if self.command(0x0006, 0, &[], &mut data, clock)? != 2 {
            return Err(Error::Length);
        }
        Ok(u16::from_le_bytes(data))
    }
    pub fn cbi_u32(&mut self, tag: CbiTag, clock: &mut impl Clock) -> Result<u32, Error> {
        let request = CbiRequest {
            tag: U32::new(tag as u32),
            flags: U32::new(0),
        };
        let mut data = [0; 4];
        let length = self.command(0x011f, 0, request.as_bytes(), &mut data, clock)?;
        // CBI integer tags explicitly allow uint32 or smaller. Preserve the
        // original zero extension, but reject empty integer responses.
        if length == 0 {
            return Err(Error::Length);
        }
        Ok(u32::from_le_bytes(data))
    }
    pub fn is_poisoned(&self) -> bool {
        self.poisoned
    }
}
#[derive(Debug, Clone, Copy)]
#[repr(u32)]
pub enum CbiTag {
    BoardVersion = 0,
    SkuId = 2,
    FirmwareConfig = 6,
    Ssfc = 8,
}

pub struct NativeIo {
    _exclusive: core::marker::PhantomData<*mut ()>,
}
impl NativeIo {
    /// # Safety
    /// Ring0, live linear LPC/eSPI EC mapping for200/204,800..8ff,900..9fe;
    /// exclusive serialized EC access, including AP/SMM paths. Not MEC EMI.
    pub unsafe fn new() -> Self {
        Self {
            _exclusive: core::marker::PhantomData,
        }
    }
}
#[cfg(target_arch = "x86_64")]
impl Io for NativeIo {
    fn read8(&mut self, port: u16) -> u8 {
        let value: u8;
        unsafe {
            core::arch::asm!("in al, dx", in("dx") port, out("al") value, options(nostack));
        }
        value
    }
    fn write8(&mut self, port: u16, value: u8) {
        unsafe {
            core::arch::asm!("out dx, al", in("dx") port, in("al") value, options(nostack));
        }
    }
}

#[cfg(test)]
mod tests;
