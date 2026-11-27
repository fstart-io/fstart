// SPDX-License-Identifier: MIT OR Apache-2.0
//! ChromeEC host protocols v2 and v3 over linear LPC/eSPI I/O.
//! This backend is not MEC EMI or RTK; those need distinct access mechanisms.
#![no_std]

#[cfg(feature = "acpi")]
pub mod acpi;
mod events;
pub mod handoff;
pub use events::EventMasks;
use fstart_core::services::Timer;
use zerocopy::{
    FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned,
    byteorder::{LittleEndian, LittleEndian as LE, U16, U32},
};
const PACKET: u16 = 0x800;
const STATUS: u16 = 0x204;
const DATA: u16 = 0x200;
const TO_HOST: u8 = 1;
const BUSY: u8 = 6; // FROM_HOST or PROCESSING
const CAPACITY: usize = 256;
/// The original (pre-UHEPI) EC host-event mask write commands.
#[repr(u16)]
#[derive(Clone, Copy)]
pub enum LegacyMask {
    Smi = 0x008a,
    Sci = 0x008b,
    Wake = 0x008e,
}

pub trait Io {
    fn read8(&mut self, port: u16) -> u8;
    fn write8(&mut self, port: u16, value: u8);
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
impl Error {
    pub const fn description(self) -> &'static str {
        match self {
            Self::Unavailable => "unavailable/busy",
            Self::UnsupportedProtocol => "unsupported protocol",
            Self::Length => "invalid length",
            Self::Timeout => "timeout",
            Self::Poisoned => "earlier submitted command failed",
            Self::Header => "bad header",
            Self::Checksum => "bad checksum",
            Self::Rejected(_) => "EC rejection",
        }
    }
    pub const fn result_code(self) -> u16 {
        if let Self::Rejected(code) = self {
            code
        } else {
            0
        }
    }
}
fn checksum(bytes: &[u8]) -> u8 {
    fstart_core::checksum::sum8(bytes)
}
fn wait(io: &mut impl Io, timer: &impl Timer) -> Result<(), Error> {
    let start = timer.timestamp_us();
    for _ in 0..=300000 {
        let status = io.read8(STATUS);
        if status == 0xff {
            return Err(Error::Unavailable);
        }
        if status & BUSY == 0 {
            return Ok(());
        }
        if timer.timestamp_us().wrapping_sub(start) >= 30_000_000 {
            break;
        }
        timer.delay_us(100);
    }
    Err(Error::Timeout)
}
// Never read an output byte to make a channel appear idle: it can belong to OSPM.
fn idle(status: u8) -> Result<(), Error> {
    if status & (TO_HOST | BUSY) == 0 {
        Ok(())
    } else {
        Err(Error::Unavailable)
    }
}
/// Exclusive serialized host-command owner. A submitted timeout/malformed
/// response poisons this owner: no duplicate mutation or silent protocol retry.
pub struct Ec<I> {
    io: I,
    v3: bool,
    query_pending: bool,
    host_pending: bool,
    poisoned: bool,
}
impl<I: Io> Ec<I> {
    /// Discover the host protocol once through the linear EC memory map. `io` must
    /// own the decoded linear LPC/eSPI windows, NOT a MEC/RTK indirect backend.
    pub fn discover(mut io: I) -> Result<Self, Error> {
        if io.read8(0x920) != b'E' || io.read8(0x921) != b'C' {
            return Err(Error::Unavailable);
        }
        let flags = io.read8(0x927);
        if flags == 0xff {
            return Err(Error::Unavailable);
        }
        if flags & 3 == 0 {
            return Err(Error::UnsupportedProtocol);
        }
        Ok(Self {
            io,
            v3: flags & 2 != 0,
            query_pending: false,
            host_pending: false,
            poisoned: false,
        })
    }
    pub fn command(
        &mut self,
        command: u16,
        version: u8,
        input: &[u8],
        output: &mut [u8],
        timer: &impl Timer,
    ) -> Result<usize, Error> {
        self.transact::<CAPACITY>(command, version, input, output, |io, _| wait(io, timer))
    }

    /// Legacy mask/charging commands with no returned payload. At an SMM
    /// ownership handoff don't spend seconds waiting for an unresponsive EC,
    /// or wait out an in-flight OS command and overwrite its response.
    fn short_command<const RX: usize>(
        &mut self,
        command: u16,
        version: u8,
        input: &[u8],
        output: &mut [u8],
        mut delay_us: impl FnMut(u32),
    ) -> Result<usize, Error> {
        self.transact::<RX>(command, version, input, output, |io, submitting| {
            if submitting {
                return idle(io.read8(STATUS));
            }
            for _ in 0..100 {
                let s = io.read8(STATUS);
                if s == 0xff {
                    return Err(Error::Unavailable);
                }
                if s & BUSY == 0 {
                    return Ok(());
                }
                delay_us(10);
            }
            Err(Error::Timeout)
        })
    }

    fn short_write(
        &mut self,
        command: u16,
        input: &[u8],
        delay: impl FnMut(u32),
    ) -> Result<(), Error> {
        self.short_command::<8>(command, 0, input, &mut [], delay)
            .map(|_| ())
    }

    pub fn short_set_mask(
        &mut self,
        mask: LegacyMask,
        events: u32,
        delay_us: impl FnMut(u32),
    ) -> Result<(), Error> {
        self.short_command::<12>(
            mask as u16,
            0,
            U32::<LE>::new(events).as_bytes(),
            &mut [0; 4],
            delay_us,
        )
        .map(|_| ())
    }

    /// EC_CMD_USB_CHARGE_SET_MODE, mode DISABLED, no inhibit flag.
    pub fn short_disable_usb_charging(
        &mut self,
        port: u8,
        delay_us: impl FnMut(u32),
    ) -> Result<(), Error> {
        self.short_write(0x0030, &[port, 0], delay_us)
    }

    /// ACPI QUERY on the independent 0x62/0x66 channel. Only query while
    /// firmware owns this channel; never steal an event from OSPM.
    pub fn query_event(&mut self, mut delay_us: impl FnMut(u32)) -> Result<u8, Error> {
        const CMD: u16 = 0x66;
        const DATA: u16 = 0x62;
        if !self.query_pending {
            idle(self.io.read8(CMD))?;
            self.io.write8(CMD, 0x84);
            self.query_pending = true;
        }
        for _ in 0..100 {
            let status = self.io.read8(CMD);
            if status == 0xff {
                return Err(Error::Unavailable);
            }
            if status & (BUSY | TO_HOST) == TO_HOST {
                self.query_pending = false;
                return Ok(self.io.read8(DATA));
            }
            delay_us(10);
        }
        Err(Error::Timeout)
    }

    // A mask write in SMM needs only the response header on its small stack.
    fn transact<const RX_BYTES: usize>(
        &mut self,
        command: u16,
        version: u8,
        input: &[u8],
        output: &mut [u8],
        mut ready: impl FnMut(&mut I, bool) -> Result<(), Error>,
    ) -> Result<usize, Error> {
        if self.poisoned {
            return Err(Error::Poisoned);
        }
        let max = CAPACITY - if self.v3 { 8 } else { 4 };
        if input.len() > max || output.len() > max {
            return Err(Error::Length);
        }
        if !self.v3 {
            return self.transact_v2::<RX_BYTES>(command, version, input, output, ready);
        }
        ready(&mut self.io, true)?;
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
        self.host_pending = true;
        self.io.write8(STATUS, 0xda);
        ready(&mut self.io, false)?;
        self.command_status()?;
        let mut bytes = [0u8; RX_BYTES];
        let (header, payload) = bytes.split_at_mut_checked(8).ok_or(Error::Length)?;
        for (i, byte) in header.iter_mut().enumerate() {
            *byte = self.io.read8(PACKET + i as u16);
        }
        let response = Response::read_from_bytes(header).map_err(|_| Error::Header)?;
        if response.version != 3 || response.reserved.get() != 0 {
            return Err(Error::Header);
        }
        let length = usize::from(response.length.get());
        let output = output.get_mut(..length).ok_or(Error::Length)?;
        let payload = payload.get_mut(..length).ok_or(Error::Length)?;
        for (i, byte) in payload.iter_mut().enumerate() {
            *byte = self.io.read8(PACKET + 8 + i as u16);
        }
        if checksum(header).wrapping_add(checksum(payload)) != 0 {
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
        for (dst, src) in output.iter_mut().zip(payload) {
            *dst = *src;
        }
        Ok(length)
    }
    fn command_status(&mut self) -> Result<(), Error> {
        let result = self.io.read8(DATA);
        self.host_pending = false;
        match result {
            0 => Ok(()),
            0xff => Err(Error::Unavailable),
            _ => {
                // IN_PROGRESS can outlive BUSY; never resubmit that request.
                self.poisoned = result == 8;
                Err(Error::Rejected(u16::from(result)))
            }
        }
    }

    fn transact_v2<const RX: usize>(
        &mut self,
        command: u16,
        version: u8,
        input: &[u8],
        output: &mut [u8],
        mut ready: impl FnMut(&mut I, bool) -> Result<(), Error>,
    ) -> Result<usize, Error> {
        #[repr(C)]
        #[derive(Clone, Copy, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
        struct Protocol2Prefix {
            flags: u8,
            version: u8,
            length: u8,
            checksum: u8,
        }

        let cmd = u8::try_from(command).map_err(|_| Error::UnsupportedProtocol)?;
        ready(&mut self.io, true)?;
        let len = input.len() as u8;
        let mut req = Protocol2Prefix {
            flags: 1,
            version,
            length: len,
            checksum: 0,
        };
        req.checksum = cmd
            .wrapping_add(checksum(req.as_bytes()))
            .wrapping_add(checksum(input));
        for (i, &b) in input.iter().enumerate() {
            self.io.write8(PACKET + 4 + i as u16, b);
        }
        for (i, &b) in req.as_bytes().iter().enumerate() {
            self.io.write8(PACKET + i as u16, b);
        }
        self.poisoned = true;
        self.host_pending = true;
        self.io.write8(STATUS, cmd);
        ready(&mut self.io, false)?;
        self.command_status()?;
        let raw_hdr: [u8; 4] = core::array::from_fn(|i| self.io.read8(PACKET + i as u16));
        let resp = Protocol2Prefix::read_from_bytes(&raw_hdr).map_err(|_| Error::Header)?;
        if resp.flags & 2 == 0 {
            return Err(Error::Header);
        }
        let mut data = [0u8; RX];
        let n = usize::from(resp.length);
        let data = data.get_mut(..n).ok_or(Error::Length)?;
        let output = output.get_mut(..n).ok_or(Error::Length)?;
        for (i, b) in data.iter_mut().enumerate() {
            *b = self.io.read8(PACKET + 4 + i as u16);
        }
        let expected_sum = cmd
            .wrapping_add(checksum(&raw_hdr[..3]))
            .wrapping_add(checksum(data));
        if expected_sum != resp.checksum {
            return Err(Error::Checksum);
        }
        for (dst, src) in output.iter_mut().zip(data) {
            *dst = *src;
        }
        self.poisoned = false;
        Ok(n)
    }

    /// A timed-out ACPI query still owns its response; the next query_event
    /// finishes it rather than submitting another QUERY or reading OS data.
    pub fn query_pending(&self) -> bool {
        self.query_pending
    }

    /// Finish *only* responses to commands issued by this owner. No request
    /// (including a QUERY) is issued here. A malformed completed response
    /// poisons the owner but isn't outstanding and doesn't need a data read.
    pub fn finish_owned(&mut self, mut delay: impl FnMut(u32)) -> Result<(), Error> {
        for _ in 0..1000 {
            if self.query_pending && self.io.read8(0x66) & (TO_HOST | BUSY) == TO_HOST {
                self.io.read8(0x62);
                self.query_pending = false;
            }
            if self.host_pending && self.io.read8(STATUS) & (TO_HOST | BUSY) == TO_HOST {
                self.io.read8(DATA);
                self.host_pending = false;
            }
            if !self.host_pending && !self.query_pending {
                return Ok(());
            }
            delay(10);
        }
        Err(Error::Timeout)
    }

    /// Legacy GET_BOARD_VERSION is command 6 with a two-byte response, NOT
    /// CBI tag0. Firmware can report different values through the two paths.
    pub fn board_version(&mut self, timer: &impl Timer) -> Result<u16, Error> {
        let mut data = [0; 2];
        if self.command(0x0006, 0, &[], &mut data, timer)? != 2 {
            return Err(Error::Length);
        }
        Ok(U16::<LE>::read_from_bytes(&data)
            .map_err(|_| Error::Length)?
            .get())
    }
    pub fn cbi_u32(&mut self, tag: CbiTag, timer: &impl Timer) -> Result<u32, Error> {
        let request = CbiRequest {
            tag: U32::new(tag as u32),
            flags: U32::new(0),
        };
        let mut data = [0; 4];
        let length = self.command(0x011f, 0, request.as_bytes(), &mut data, timer)?;
        // CBI integer tags explicitly allow uint32 or smaller. Preserve the
        // original zero extension, but reject empty integer responses.
        if length == 0 {
            return Err(Error::Length);
        }
        Ok(U32::<LE>::read_from_bytes(&data)
            .map_err(|_| Error::Length)?
            .get())
    }
    pub fn set_mask(
        &mut self,
        kind: LegacyMask,
        events: u32,
        timer: &impl Timer,
    ) -> Result<(), Error> {
        // Older LPC handlers may return the mask, or no payload, for SET.
        self.command(
            kind as u16,
            0,
            U32::<LE>::new(events).as_bytes(),
            &mut [0; 4],
            timer,
        )
        .map(|_| ())
    }

    /// The host events that raise the EC's SCI (`HOST_EVENT_SET_SCI_MASK`),
    /// each the [`HostEvent::mask`] of an event. The OS reads which one in
    /// the EC's ACPI query and runs its `_Qxx`.
    pub fn set_sci_mask(&mut self, mask: u32, timer: &impl Timer) -> Result<(), Error> {
        self.set_mask(LegacyMask::Sci, mask, timer)
    }

    /// The host events that wake the system (`HOST_EVENT_SET_WAKE_MASK`).
    pub fn set_wake_mask(&mut self, mask: u32, timer: &impl Timer) -> Result<(), Error> {
        self.set_mask(LegacyMask::Wake, mask, timer)
    }

    pub fn is_poisoned(&self) -> bool {
        self.poisoned
    }
}
/// The EC's host events (`ec_commands.h`, `enum host_event_code`) the
/// ACPI code of [`acpi`] handles; the number is its `_Qxx` query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum HostEvent {
    LidClosed = 1,
    LidOpen = 2,
    PowerButton = 3,
    AcConnected = 4,
    AcDisconnected = 5,
    BatteryLow = 6,
    BatteryCritical = 7,
    Battery = 8,
    ThermalThreshold = 9,
    ThermalOverload = 10,
    Thermal = 11,
    KeyPressed = 13,
    ThermalShutdown = 16,
    BatteryShutdown = 17,
    ThrottleStart = 18,
    ThrottleStop = 19,
    PdMcu = 22,
    BatteryStatus = 23,
    Panic = 24,
    Mkbp = 27,
    UsbMux = 28,
    ModeChange = 29,
}

impl HostEvent {
    /// Queries emitted by the thermal-aware LPC AML, also needed in SMM
    /// builds which do not link the AML generator.
    pub const THERMAL_EVENTS: u32 = Self::ThermalThreshold.mask()
        | Self::ThermalOverload.mask()
        | Self::Thermal.mask()
        | Self::ThermalShutdown.mask()
        | Self::ThrottleStart.mask()
        | Self::ThrottleStop.mask();

    /// The event's bit in a host-event mask.
    pub const fn mask(self) -> u32 {
        1 << (self as u8 - 1)
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
    /// exclusive serialized EC access, including AP/SMM paths. OSPM must finish
    /// all multi-byte ACPI and host transactions, consume the replies and stop
    /// issuing requests before an SMM handoff. Idle status alone cannot prove
    /// that no transaction is between bytes. Not MEC EMI.
    pub unsafe fn new() -> Self {
        Self {
            _exclusive: core::marker::PhantomData,
        }
    }
}
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
impl Io for NativeIo {
    fn read8(&mut self, port: u16) -> u8 {
        // SAFETY: the constructor owns the EC port ranges.
        unsafe { fstart_core::pio::inb(port) }
    }
    fn write8(&mut self, port: u16, value: u8) {
        // SAFETY: the constructor owns the EC port ranges.
        unsafe { fstart_core::pio::outb(port, value) }
    }
}

#[cfg(test)]
mod tests;
