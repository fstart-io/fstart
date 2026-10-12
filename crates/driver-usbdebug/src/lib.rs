//! Polled EHCI Appendix C debug console. No DMA, allocator or USB schedule.
//!
//! Requires a directly attached high-speed USB debug gadget (debug descriptor,
//! not an ordinary USB serial adapter). The platform enables ECAM and must
//! reserve `Config::fixed_bar()` from PCI allocation. Each stage re-enumerates
//! the gadget, resetting endpoint toggles rather than relying on CAR state.
//! A missing/broken gadget leaves the console offline, never stops boot.

#![no_std]

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, Ordering};
use fstart_core::mmio::{MmioReadOnly, MmioReadWrite};
use fstart_core::services::{Console, ConsoleDevice, ServiceError};
use fstart_pci::{PciAddress, PciFixedBar, PciFixedBarType, ecam};
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};
use tock_registers::{register_bitfields, register_structs};
use zerocopy::byteorder::{LE, U16};
use zerocopy::{FromBytes, Immutable, IntoBytes};

register_bitfields![u32,
    Command [RUN OFFSET(0) NUMBITS(1) [], RESET OFFSET(1) NUMBITS(1) []],
    Status [HALTED OFFSET(12) NUMBITS(1) []],
    Structural [PORTS OFFSET(0) NUMBITS(4) [], PPC OFFSET(4) NUMBITS(1) [],
        DEBUG_PORT OFFSET(20) NUMBITS(4) []],
    Port [CONNECTED OFFSET(0) NUMBITS(1) [], ENABLED OFFSET(2) NUMBITS(1) [],
        RESET OFFSET(8) NUMBITS(1) [], POWER OFFSET(12) NUMBITS(1) []],
    Control [LENGTH OFFSET(0) NUMBITS(4) [], OUT OFFSET(4) NUMBITS(1) [],
        GO OFFSET(5) NUMBITS(1) [], ERROR OFFSET(6) NUMBITS(1) [],
        ERROR_CODE OFFSET(7) NUMBITS(3) [], IN_USE OFFSET(10) NUMBITS(1) [],
        DONE OFFSET(16) NUMBITS(1) [], ENABLED OFFSET(28) NUMBITS(1) [],
        OWNER OFFSET(30) NUMBITS(1) []],
    Pids [TOKEN OFFSET(0) NUMBITS(8) [], DATA OFFSET(8) NUMBITS(8) [],
        RECEIVED OFFSET(16) NUMBITS(8) []],
    Address [ENDPOINT OFFSET(0) NUMBITS(4) [], DEVICE OFFSET(8) NUMBITS(7) []]
];
register_structs! {
    Caps {
        (0x00 => capbase: MmioReadOnly<u32>),
        (0x04 => structural: MmioReadOnly<u32, Structural::Register>),
        (0x08 => @END),
    },
    Operational {
        (0x00 => command: MmioReadWrite<u32, Command::Register>),
        (0x04 => status: MmioReadOnly<u32, Status::Register>),
        (0x08 => interrupts: MmioReadWrite<u32>),
        (0x0c => _reserved),
        (0x40 => configured: MmioReadWrite<u32>),
        (0x44 => @END),
    },
    DebugPort {
        (0x00 => control: MmioReadWrite<u32, Control::Register>),
        (0x04 => pids: MmioReadWrite<u32, Pids::Register>),
        (0x08 => data: [MmioReadWrite<u32>; 2]),
        (0x10 => address: MmioReadWrite<u32, Address::Register>),
        (0x14 => @END),
    }
}

const DATA0: u8 = 0xc3;
const DATA1: u8 = 0x4b;
const OUT: u8 = 0xe1;
const IN: u8 = 0x69;
const SETUP: u8 = 0x2d;
const ACK: u8 = 0xd2;
const NAK: u8 = 0x5a;
const NYET: u8 = 0x96;
const DEVICE: u8 = 127;
const PORT_W1C: u32 = (1 << 1) | (1 << 3) | (1 << 5);
const CLAIM: u32 = (1 << 30) | (1 << 28) | (1 << 10);
const BAR_SIZE: u64 = 0x1000;

/// Fixed console wiring. ECAM is already enabled by the chipset. Its address
/// is needed again in postcar, whose ECAM accessor starts with fresh BSS.
#[derive(Debug, Clone, Copy)]
pub struct Config {
    pub ecam_base: usize,
    pub bus: u8,
    pub device: u8,
    pub function: u8,
    pub bar: u32,
}

impl Config {
    pub fn fixed_bar(self) -> PciFixedBar {
        PciFixedBar {
            address: PciAddress::new(0, self.bus, self.device, self.function),
            register: 0x10,
            kind: PciFixedBarType::Memory32,
            base: self.bar as u64,
            size: BAR_SIZE,
            prefetchable: false,
        }
    }
}

#[repr(C)]
#[derive(FromBytes, IntoBytes, Immutable)]
struct SetupPacket {
    request_type: u8,
    request: u8,
    value: U16<LE>,
    index: U16<LE>,
    length: U16<LE>,
}

struct State {
    debug: usize,
    output: u8,
    input: u8,
    out_pid: u8,
    in_pid: u8,
    rx: [u8; 8],
    rx_pos: usize,
    rx_len: usize,
    online: bool,
}

impl State {
    const fn new() -> Self {
        Self {
            debug: 0,
            output: 0,
            input: 0,
            out_pid: DATA0,
            in_pid: DATA0,
            rx: [0; 8],
            rx_pos: 0,
            rx_len: 0,
            online: false,
        }
    }

    fn regs(&self) -> &DebugPort {
        // SAFETY: init validates the debug capability's BAR/offset before use.
        unsafe { &*(self.debug as *const DebugPort) }
    }

    /// Return None for device backpressure, error for stalled/failed hardware.
    /// DATA toggle changes only on ACK (OUT) or matching DATA (IN).
    fn packet(
        &self,
        device: u8,
        endpoint: u8,
        token: u8,
        pid: &mut u8,
        bytes: &mut [u8],
        attempts: usize,
    ) -> Result<Option<usize>, ServiceError> {
        self.packet_with_delay(
            (device, endpoint),
            token,
            pid,
            bytes,
            attempts,
            fstart_arch::udelay,
        )
    }

    fn packet_with_delay(
        &self,
        (device, endpoint): (u8, u8),
        token: u8,
        pid: &mut u8,
        bytes: &mut [u8],
        attempts: usize,
        mut delay: impl FnMut(u32),
    ) -> Result<Option<usize>, ServiceError> {
        if bytes.len() > 8 {
            return Err(ServiceError::InvalidParam);
        }
        let r = self.regs();
        r.address
            .write(Address::DEVICE.val(device.into()) + Address::ENDPOINT.val(endpoint.into()));
        r.pids
            .write(Pids::TOKEN.val(token.into()) + Pids::DATA.val((*pid).into()));
        if token != IN {
            let packed = pack(bytes);
            r.data[0].set(packed[0]);
            r.data[1].set(packed[1]);
        }
        let ctrl = CLAIM
            | Control::LENGTH.val(bytes.len() as u32).value
            | if token != IN {
                Control::OUT::SET.value
            } else {
                0
            };
        for _ in 0..attempts {
            // DONE is W1C: clear stale completion before asserting GO.
            r.control.set(ctrl | Control::DONE::SET.value);
            r.control.set(ctrl | Control::GO::SET.value);
            let mut complete = false;
            for _ in 0..1000 {
                if r.control.is_set(Control::DONE) {
                    complete = true;
                    break;
                }
                delay(1);
            }
            if !complete {
                return Err(ServiceError::Timeout);
            }
            let observed = r.control.extract();
            r.control.set(ctrl | Control::DONE::SET.value);
            if observed.is_set(Control::ERROR) {
                if observed.read(Control::ERROR_CODE) == 1 {
                    continue;
                }
                return Err(ServiceError::IoError);
            }
            let reply = r.pids.read(Pids::RECEIVED) as u8;
            if accepted(token, *pid, reply) {
                let length = observed.read(Control::LENGTH) as usize;
                if length > bytes.len() || length > 8 {
                    return Err(ServiceError::IoError);
                }
                if token != IN && length != bytes.len() {
                    return Err(ServiceError::IoError);
                }
                if token == IN {
                    unpack([r.data[0].get(), r.data[1].get()], &mut bytes[..length]);
                }
                *pid ^= DATA0 ^ DATA1;
                return Ok(Some(length));
            }
            if !matches!(reply, NAK | NYET) && !(token == IN && reply == (*pid ^ (DATA0 ^ DATA1))) {
                return Err(ServiceError::IoError);
            }
            delay(10);
        }
        Ok(None)
    }

    fn control(
        &self,
        device: u8,
        request_type: u8,
        request: u8,
        value: u16,
        data: &mut [u8],
    ) -> Result<usize, ServiceError> {
        let setup = SetupPacket {
            request_type,
            request,
            value: value.into(),
            index: 0.into(),
            length: (data.len() as u16).into(),
        };
        let mut bytes = [0; 8];
        bytes.copy_from_slice(setup.as_bytes());
        let mut pid = DATA0;
        if self.packet(device, 0, SETUP, &mut pid, &mut bytes, 32)? != Some(8) {
            return Err(ServiceError::Timeout);
        }
        let input = request_type & 0x80 != 0;
        let length = if data.is_empty() {
            0
        } else {
            self.packet(device, 0, if input { IN } else { OUT }, &mut pid, data, 32)?
                .ok_or(ServiceError::Timeout)?
        };
        let mut status_pid = DATA1;
        if self.packet(
            device,
            0,
            if input { OUT } else { IN },
            &mut status_pid,
            &mut [],
            32,
        )? != Some(0)
        {
            return Err(ServiceError::IoError);
        }
        Ok(length)
    }

    fn enumerate(&mut self) -> Result<(), ServiceError> {
        let (input, output) = debug_endpoints(|device, ty, request, value, data| {
            self.control(device, ty, request, value, data)
        })?;
        self.input = input;
        self.output = output;
        let mut pid = DATA0;
        let mut greeting = *b"USB\r\n";
        // Some FX2 gadgets also require SET_CONFIGURATION before bulk works.
        if self
            .packet(DEVICE, self.output, OUT, &mut pid, &mut greeting, 32)
            .ok()
            .flatten()
            != Some(5)
        {
            self.control(DEVICE, 0, 9, 1, &mut [])?;
            pid = DATA0;
            if self.packet(DEVICE, self.output, OUT, &mut pid, &mut greeting, 32)? != Some(5) {
                return Err(ServiceError::Timeout);
            }
        }
        self.out_pid = pid;
        self.in_pid = DATA0;
        self.online = true;
        Ok(())
    }
}

// Gadgets may retain the reserved debug address across a port reset. Keep
// recovery bounded and skip SET_ADDRESS when already at that address.
fn debug_endpoints(
    mut control: impl FnMut(u8, u8, u8, u16, &mut [u8]) -> Result<usize, ServiceError>,
) -> Result<(u8, u8), ServiceError> {
    let mut desc = [0; 4];
    let (device, length) = match control(0, 0x80, 6, 10 << 8, &mut desc) {
        Ok(length) => (0, length),
        Err(_) => (DEVICE, control(DEVICE, 0x80, 6, 10 << 8, &mut desc)?),
    };
    if length != 4
        || desc[0..2] != [4, 10]
        || desc[2] & 0x7f == 0
        || desc[3] == 0
        || desc[2] & 0x70 != 0
        || desc[3] & 0xf0 != 0
    {
        return Err(ServiceError::NotSupported);
    }
    if device == 0 {
        control(0, 0, 5, DEVICE.into(), &mut [])?;
        fstart_arch::udelay(2000);
    }
    control(DEVICE, 0, 3, 6, &mut [])?; // DEVICE_DEBUG_MODE
    Ok((desc[2] & 0x0f, desc[3]))
}

fn accepted(token: u8, pid: u8, reply: u8) -> bool {
    if token == IN {
        reply == pid
    } else {
        reply == ACK
    }
}

fn pack(bytes: &[u8]) -> [u32; 2] {
    bytes
        .iter()
        .enumerate()
        .fold([0; 2], |mut words, (index, byte)| {
            words[index / 4] |= u32::from(*byte) << (8 * (index % 4));
            words
        })
}
fn unpack(words: [u32; 2], bytes: &mut [u8]) {
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = (words[index / 4] >> (8 * (index % 4))) as u8;
    }
}

/// Bounded PCI capability walk: reject cycles, bad pointers and BAR selectors.
fn capabilities(dev: ecam::EcamDevice) -> Result<(u16, Option<u16>), ServiceError> {
    if dev.read16(6) & (1 << 4) == 0 {
        return Err(ServiceError::NotSupported);
    }
    let mut next = dev.read8(0x34);
    let mut seen = 0u64;
    let mut debug = None;
    let mut pm = None;
    while next != 0 {
        if next < 0x40 || next & 3 != 0 {
            return Err(ServiceError::HardwareError);
        }
        let bit = 1u64 << (next / 4);
        if seen & bit != 0 {
            return Err(ServiceError::HardwareError);
        }
        seen |= bit;
        match dev.read8(next.into()) {
            0x0a => debug = Some(u16::from(next)),
            0x01 if next <= 0xf8 => pm = Some(u16::from(next)),
            _ => (),
        }
        next = dev.read8(u16::from(next) + 1);
    }
    Ok((debug.ok_or(ServiceError::NotSupported)?, pm))
}

pub struct EhciDebug {
    config: Config,
    busy: AtomicBool,
    state: UnsafeCell<State>,
}
// SAFETY: all shared state and MMIO transactions are serialized by busy.
unsafe impl Send for EhciDebug {}
unsafe impl Sync for EhciDebug {}

impl EhciDebug {
    /// Failure recorded by init; board code can inspect this without logging
    /// recursively through the very console being initialized.
    pub fn online(&self) -> bool {
        self.with_state(|s| Ok(s.online)).unwrap_or(false)
    }

    fn with_state<T>(
        &self,
        f: impl FnOnce(&mut State) -> Result<T, ServiceError>,
    ) -> Result<T, ServiceError> {
        // Do not spin on recursive/SMM logging: the interrupted owner cannot
        // release its lock while this caller is waiting.
        if self
            .busy
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            return Err(ServiceError::IoError);
        }
        struct Unlock<'a>(&'a AtomicBool);
        impl Drop for Unlock<'_> {
            fn drop(&mut self) {
                self.0.store(false, Ordering::Release);
            }
        }
        let _guard = Unlock(&self.busy);
        // SAFETY: busy exclusively guards this cell for the closure's lifetime.
        f(unsafe { &mut *self.state.get() })
    }

    fn initialize(&mut self) -> Result<(), ServiceError> {
        if ecam::base() == 0 {
            ecam::init(self.config.ecam_base);
        }
        if ecam::base() != self.config.ecam_base {
            return Err(ServiceError::InvalidParam);
        }
        let dev = ecam::EcamDevice::new(self.config.bus, self.config.device, self.config.function);
        if !dev.is_present() || dev.read32(8) >> 8 != 0x0c0320 {
            return Err(ServiceError::NoDevice);
        }
        let (debug, pm) = capabilities(dev)?;
        let cap = dev.read32(debug);
        // The operational registers and debug port must both be in BAR0.
        let offset = ((cap >> 16) & 0x1ffc) as usize;
        if cap >> 29 != 1 || offset < 0x10 || offset + 0x14 > BAR_SIZE as usize {
            return Err(ServiceError::NotSupported);
        }
        if let Some(pm) = pm {
            dev.and16(pm + 4, !((1 << 8) | 3));
        }
        dev.and16(4, !2);
        dev.write32(0x10, self.config.bar);
        dev.or16(4, 6);
        let base = self.config.bar as usize;
        // SAFETY: BAR0 mapped, enabled and reserved by platform policy.
        let caps = unsafe { &*(base as *const Caps) };
        let length = (caps.capbase.get() & 0xff) as usize;
        let ports = caps.structural.read(Structural::PORTS) as usize;
        let debug_port = caps.structural.read(Structural::DEBUG_PORT) as usize;
        if length < 0x10
            || length & 3 != 0
            || debug_port == 0
            || debug_port > ports
            || length + 0x44 + ports * 4 > offset
        {
            return Err(ServiceError::HardwareError);
        }
        let op = unsafe { &*((base + length) as *const Operational) };
        let port = unsafe {
            &*((base + length + 0x44 + (debug_port - 1) * 4)
                as *const MmioReadWrite<u32, Port::Register>)
        };
        let s = self.state.get_mut();
        s.debug = base + offset;
        op.command.modify(Command::RUN::CLEAR);
        wait(|| op.status.is_set(Status::HALTED), 100)?;
        op.command.modify(Command::RESET::SET);
        wait(|| !op.command.is_set(Command::RESET), 100)?;
        s.regs().control.write(Control::OWNER::SET);
        op.interrupts.set(0);
        // Keep both DMA schedules disabled. Only debug-port transactions run.
        op.command.set(Command::RUN::SET.value);
        op.configured.set(1);
        wait(|| !op.status.is_set(Status::HALTED), 100)?;
        if caps.structural.is_set(Structural::PPC) {
            port.set((port.get() & !PORT_W1C) | Port::POWER::SET.value);
        }
        wait(|| port.is_set(Port::CONNECTED), 200)?;
        fstart_arch::udelay(100_000); // connection debounce
        port.set((port.get() & !(PORT_W1C | Port::ENABLED::SET.value)) | Port::RESET::SET.value);
        fstart_arch::udelay(50_000);
        port.set(port.get() & !(PORT_W1C | Port::RESET::SET.value));
        wait(|| !port.is_set(Port::RESET), 100)?;
        if !port.is_set(Port::CONNECTED) || !port.is_set(Port::ENABLED) {
            return Err(ServiceError::NoDevice); // not a directly attached HS device
        }
        s.regs().control.set(CLAIM);
        if s.regs().control.get() & CLAIM != CLAIM {
            return Err(ServiceError::HardwareError);
        }
        fstart_arch::udelay(100_000);
        s.enumerate()
    }
}

fn wait(mut ready: impl FnMut() -> bool, milliseconds: usize) -> Result<(), ServiceError> {
    for _ in 0..milliseconds {
        if ready() {
            return Ok(());
        }
        fstart_arch::udelay(1000);
    }
    Err(ServiceError::Timeout)
}

impl ConsoleDevice for EhciDebug {
    type Config = Config;
    const NAME: &'static str = "EHCI debug";
    fn new(config: Config) -> Result<Self, ServiceError> {
        if config.bar == 0
            || config.bar & 0xfff != 0
            || config.ecam_base == 0
            || config.ecam_base & 0xfffff != 0
            || config.device > 31
            || config.function > 7
        {
            return Err(ServiceError::InvalidParam);
        }
        Ok(Self {
            config,
            busy: AtomicBool::new(false),
            state: UnsafeCell::new(State::new()),
        })
    }
    fn init(&mut self) -> Result<(), ServiceError> {
        *self.state.get_mut() = State::new();
        if self.initialize().is_err() {
            let s = self.state.get_mut();
            if s.debug != 0 {
                s.regs().control.set(0);
            }
            s.online = false;
        }
        Ok(())
    }
}

impl Console for EhciDebug {
    fn write_byte(&self, byte: u8) -> Result<(), ServiceError> {
        self.write_bytes(&[byte])
    }
    fn write_bytes(&self, bytes: &[u8]) -> Result<(), ServiceError> {
        self.with_state(|s| {
            if !s.online {
                return Err(ServiceError::NotInitialized);
            }
            for chunk in bytes.chunks(8) {
                let mut data = [0; 8];
                data[..chunk.len()].copy_from_slice(chunk);
                let mut pid = s.out_pid;
                match s.packet(
                    DEVICE,
                    s.output,
                    OUT,
                    &mut pid,
                    &mut data[..chunk.len()],
                    32,
                ) {
                    Ok(Some(_)) => s.out_pid = pid,
                    Ok(None) => return Err(ServiceError::Timeout),
                    Err(error) => {
                        s.online = false;
                        return Err(error);
                    }
                }
            }
            Ok(())
        })
    }
    fn read_byte(&self) -> Result<Option<u8>, ServiceError> {
        self.with_state(|s| {
            if !s.online {
                return Ok(None);
            }
            if s.rx_pos == s.rx_len {
                let mut data = [0; 8];
                let mut pid = s.in_pid;
                let Some(length) = s.packet(DEVICE, s.input, IN, &mut pid, &mut data, 1)? else {
                    return Ok(None);
                };
                s.in_pid = pid;
                s.rx = data;
                s.rx_pos = 0;
                s.rx_len = length;
                if length == 0 {
                    return Ok(None);
                }
            }
            let byte = s.rx[s.rx_pos];
            s.rx_pos += 1;
            Ok(Some(byte))
        })
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;

    #[test]
    fn capability_walk_is_bounded_and_absent_controller_is_optional() {
        use std::alloc::{Layout, alloc_zeroed, dealloc};
        let layout = Layout::from_size_align(1 << 20, 1 << 20).unwrap();
        let memory = unsafe { alloc_zeroed(layout) };
        assert!(!memory.is_null());
        struct Mapping {
            memory: *mut u8,
            layout: Layout,
            old: usize,
        }
        impl Drop for Mapping {
            fn drop(&mut self) {
                ecam::init(self.old);
                unsafe {
                    dealloc(self.memory, self.layout);
                }
            }
        }
        let _mapping = Mapping {
            memory,
            layout,
            old: ecam::base(),
        };
        ecam::init(memory as usize);
        let dev = ecam::EcamDevice::new(0, 0, 0);
        dev.write16(6, 1 << 4);
        dev.write8(0x34, 0x40);
        dev.write16(0x40, 0x5001); // PM -> debug
        dev.write16(0x50, 0x000a);
        assert_eq!(capabilities(dev).unwrap(), (0x50, Some(0x40)));
        dev.write8(0x51, 0x40); // cycle
        assert!(matches!(
            capabilities(dev),
            Err(ServiceError::HardwareError)
        ));
        dev.write8(0x51, 0x43); // unaligned capability
        assert!(matches!(
            capabilities(dev),
            Err(ServiceError::HardwareError)
        ));
        dev.write8(0x34, 0);
        assert!(matches!(capabilities(dev), Err(ServiceError::NotSupported)));
        dev.write16(0, 0xffff);
        let mut console = EhciDebug::new(Config {
            ecam_base: memory as usize,
            bus: 0,
            device: 0,
            function: 0,
            bar: 0xfed0_4000,
        })
        .unwrap();
        assert!(console.init().is_ok());
        assert!(!console.online());
        assert!(matches!(
            console.write_byte(b'x'),
            Err(ServiceError::NotInitialized)
        ));
        assert_eq!(console.read_byte().unwrap(), None);
    }

    #[test]
    fn stuck_debug_engine_times_out_without_advancing_toggle() {
        let mut words = [0u32; 5];
        let mut state = State::new();
        state.debug = words.as_mut_ptr() as usize;
        let mut pid = DATA0;
        let mut delays = 0;
        assert!(matches!(
            state.packet_with_delay((127, 1), OUT, &mut pid, &mut [0], 32, |us| {
                assert_eq!(us, 1);
                delays += 1;
            }),
            Err(ServiceError::Timeout)
        ));
        assert_eq!(delays, 1000);
        assert_eq!(pid, DATA0);
    }

    #[test]
    fn fixed_bar_and_nonblocking_lock_policy() {
        let mut config = Config {
            ecam_base: 0xe000_0000,
            bus: 0,
            device: 0x1a,
            function: 7,
            bar: 0xfed0_4000,
        };
        let console = EhciDebug::new(config).unwrap();
        assert_eq!(config.fixed_bar().address, PciAddress::new(0, 0, 0x1a, 7));
        assert_eq!(config.fixed_bar().size, 0x1000);
        console.busy.store(true, Ordering::Relaxed);
        assert!(matches!(console.write_byte(0), Err(ServiceError::IoError)));
        config.bar += 1;
        assert!(matches!(
            EhciDebug::new(config),
            Err(ServiceError::InvalidParam)
        ));
    }
    #[test]
    fn enumeration_recovers_reserved_address_without_reassigning_it() {
        for retained in [false, true] {
            let mut descriptors = 0;
            let mut assignments = 0;
            let endpoints =
                debug_endpoints(
                    |device, ty, request, value, data| match (ty, request, value) {
                        (0x80, 6, 0x0a00) => {
                            descriptors += 1;
                            if retained && device == 0 {
                                return Err(ServiceError::Timeout);
                            }
                            assert_eq!(device, if retained { DEVICE } else { 0 });
                            data.copy_from_slice(&[4, 10, 0x81, 2]);
                            Ok(4)
                        }
                        (0, 5, 127) => {
                            assert!(!retained);
                            assignments += 1;
                            Ok(0)
                        }
                        (0, 3, 6) => {
                            assert_eq!(device, DEVICE);
                            Ok(0)
                        }
                        _ => panic!("unexpected control request"),
                    },
                );
            assert_eq!(endpoints, Ok((1, 2)));
            assert_eq!(descriptors, if retained { 2 } else { 1 });
            assert_eq!(assignments, usize::from(!retained));
        }
        assert!(debug_endpoints(|_, _, _, _, _| Err(ServiceError::Timeout)).is_err());
    }

    #[test]
    fn debug_registers_and_setup_packet_match_spec() {
        assert_eq!(core::mem::offset_of!(DebugPort, address), 0x10);
        assert_eq!(core::mem::offset_of!(Operational, configured), 0x40);
        let setup = SetupPacket {
            request_type: 0x80,
            request: 6,
            value: 0x0a00.into(),
            index: 0.into(),
            length: 4.into(),
        };
        assert_eq!(setup.as_bytes(), &[0x80, 6, 0, 10, 0, 0, 4, 0]);
    }
    #[test]
    fn packets_pack_all_eight_bytes_in_little_endian_order() {
        let bytes = [0, 1, 2, 3, 4, 5, 6, 7];
        assert_eq!(pack(&bytes), [0x03020100, 0x07060504]);
        let mut unpacked = [0; 8];
        unpack(pack(&bytes), &mut unpacked);
        assert_eq!(unpacked, bytes);
        assert_eq!(pack(&bytes[..3]), [0x00020100, 0]);
    }
    #[test]
    fn toggle_advances_only_for_the_correct_direction_and_response() {
        assert!(accepted(OUT, DATA0, ACK));
        assert!(accepted(IN, DATA1, DATA1));
        assert!(!accepted(IN, DATA1, DATA0));
        assert!(!accepted(IN, DATA1, ACK));
        assert!(!accepted(OUT, DATA0, DATA0));
        assert!(!accepted(OUT, DATA0, NAK));
        assert!(!accepted(OUT, DATA0, NYET));
    }
}
