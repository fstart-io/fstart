//! Polled EHCI Appendix C debug console (coreboot `drivers/usb/ehci_debug.c`).
//! No DMA, allocator or USB schedule.
//!
//! Requires a high-speed USB debug gadget with the debug descriptor (not an
//! ordinary USB serial adapter) directly on the controller's debug port. The
//! platform enables ECAM and reserves `Config::fixed_bar()` from PCI
//! allocation, so the controller keeps its BAR for the whole boot.
//!
//! The first stage resets the controller, resets the debug port and
//! enumerates the gadget at address 127. Later stages find the debug port
//! still claimed and running and only re-attach: they read the endpoints
//! back from the gadget and resynchronize the data toggles with
//! `CLEAR_FEATURE(ENDPOINT_HALT)`, instead of a port reset that would cost
//! every stage a quarter second. coreboot carries the same state through
//! CAR and CBMEM.
//!
//! Output is sent in 8-byte packets, the debug port's maximum, and flushed
//! by [`Console::flush`] (coreboot `usb_tx_byte`/`usb_tx_flush`). A missing or
//! broken gadget leaves the console offline; it never stops the boot.

#![no_std]

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, Ordering};
use fstart_core::mmio::{MmioReadOnly, MmioReadWrite};
use fstart_core::services::{Console, ConsoleDevice, ServiceError};
use fstart_pci::{
    PCI_COMMAND_BITS, PciAddress, PciFixedBar, PciFixedBarType, PciType0Config, ecam,
};
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};
use tock_registers::{LocalRegisterCopy, register_bitfields, register_structs};
use zerocopy::byteorder::{LE, U16};
use zerocopy::{FromBytes, Immutable, IntoBytes};

register_bitfields![u32,
    /// USBCMD.
    Command [
        RUN OFFSET(0) NUMBITS(1) [],
        RESET OFFSET(1) NUMBITS(1) [],
        PERIODIC OFFSET(4) NUMBITS(1) [],
        ASYNC OFFSET(5) NUMBITS(1) [],
        ASYNC_DOORBELL OFFSET(6) NUMBITS(1) [],
        LIGHT_RESET OFFSET(7) NUMBITS(1) []
    ],
    /// USBSTS.
    Status [HALTED OFFSET(12) NUMBITS(1) []],
    /// HCSPARAMS.
    Structural [
        PORTS OFFSET(0) NUMBITS(4) [],
        PPC OFFSET(4) NUMBITS(1) [],
        DEBUG_PORT OFFSET(20) NUMBITS(4) []
    ],
    /// CONFIGFLAG: route every port to EHCI.
    Configured [CF OFFSET(0) NUMBITS(1) []],
    /// PORTSC. CSC, PEC and OCC are write-1-to-clear.
    Port [
        CONNECTED OFFSET(0) NUMBITS(1) [],
        CONNECT_CHANGE OFFSET(1) NUMBITS(1) [],
        ENABLED OFFSET(2) NUMBITS(1) [],
        ENABLE_CHANGE OFFSET(3) NUMBITS(1) [],
        OVERCURRENT_CHANGE OFFSET(5) NUMBITS(1) [],
        RESET OFFSET(8) NUMBITS(1) [],
        POWER OFFSET(12) NUMBITS(1) []
    ],
    /// Debug port control/status (EHCI C.3.1).
    Control [
        LENGTH OFFSET(0) NUMBITS(4) [],
        OUT OFFSET(4) NUMBITS(1) [],
        GO OFFSET(5) NUMBITS(1) [],
        ERROR OFFSET(6) NUMBITS(1) [],
        EXCEPTION OFFSET(7) NUMBITS(3) [
            None = 0,
            Transaction = 1,
            Hardware = 2
        ],
        IN_USE OFFSET(10) NUMBITS(1) [],
        DONE OFFSET(16) NUMBITS(1) [],
        ENABLED OFFSET(28) NUMBITS(1) [],
        OWNER OFFSET(30) NUMBITS(1) []
    ],
    Pids [
        TOKEN OFFSET(0) NUMBITS(8) [],
        DATA OFFSET(8) NUMBITS(8) [],
        RECEIVED OFFSET(16) NUMBITS(8) []
    ],
    Address [
        ENDPOINT OFFSET(0) NUMBITS(4) [],
        DEVICE OFFSET(8) NUMBITS(7) []
    ],
    /// The debug port PCI capability (EHCI C.2.1).
    DebugCapability [
        ID OFFSET(0) NUMBITS(8) [],
        OFFSET OFFSET(16) NUMBITS(13) [],
        BAR OFFSET(29) NUMBITS(3) []
    ]
];
register_bitfields![u16,
    /// Power management control/status.
    PowerControl [
        STATE OFFSET(0) NUMBITS(2) [D0 = 0],
        PME_ENABLE OFFSET(8) NUMBITS(1) []
    ]
];
register_structs! {
    Caps {
        (0x00 => length: MmioReadOnly<u8>),
        (0x01 => _reserved),
        (0x04 => structural: MmioReadOnly<u32, Structural::Register>),
        (0x08 => @END),
    },
    Operational {
        (0x00 => command: MmioReadWrite<u32, Command::Register>),
        (0x04 => status: MmioReadOnly<u32, Status::Register>),
        (0x08 => interrupts: MmioReadWrite<u32>),
        (0x0c => _reserved),
        (0x40 => configured: MmioReadWrite<u32, Configured::Register>),
        (0x44 => ports: [MmioReadWrite<u32, Port::Register>; 15]),
        (0x80 => @END),
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
/// The debug device's address (USB_DEBUG_DEVNUM).
const DEVICE: u8 = 127;
const BAR_SIZE: u64 = 0x1000;
const CAP_ID_PM: u8 = 0x01;
const CAP_ID_DEBUG_PORT: u8 = 0x0a;
const EHCI_CLASS: u32 = 0x0c0320;

/// USB standard requests and features.
const GET_DESCRIPTOR: u8 = 6;
const SET_ADDRESS: u8 = 5;
const SET_FEATURE: u8 = 3;
const CLEAR_FEATURE: u8 = 1;
const SET_CONFIGURATION: u8 = 9;
const DEBUG_DESCRIPTOR: u16 = 10 << 8;
const DEVICE_DEBUG_MODE: u16 = 6;
const ENDPOINT_HALT: u16 = 0;
const TO_DEVICE: u8 = 0x00;
const FROM_DEVICE: u8 = 0x80;
const TO_ENDPOINT: u8 = 0x02;

/// coreboot `DBGP_MICROFRAME_RETRIES`: bus errors retried per transaction.
const HOST_RETRIES: usize = 10;
/// NAK/NYET retries of a control stage.
const CONTROL_RETRIES: usize = 32;
/// NAK/NYET retries of a console packet while the host drains them
/// (coreboot's console pipe timeout).
const BULK_RETRIES: usize = 1000;
/// NAK/NYET retries once a packet was dropped: a host that stopped reading
/// must not stretch every following packet to the full budget.
const BULK_RETRIES_STALLED: usize = 8;

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

/// The controller's register blocks in BAR0.
struct Controller {
    caps: &'static Caps,
    op: &'static Operational,
    debug: usize,
}

impl Controller {
    fn port(&self) -> &MmioReadWrite<u32, Port::Register> {
        let index = self.caps.structural.read(Structural::DEBUG_PORT) as usize;
        &self.op.ports[index - 1]
    }
}

struct State {
    debug: usize,
    output: u8,
    input: u8,
    out_pid: u8,
    in_pid: u8,
    out_retries: usize,
    tx: [u8; 8],
    tx_len: usize,
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
            out_retries: BULK_RETRIES,
            tx: [0; 8],
            tx_len: 0,
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

    /// One transaction (coreboot `dbgp_wait_until_done`). `Ok(None)` when
    /// the device kept NAKing; an error for a stalled or failed transfer.
    /// The DATA toggle advances only on ACK (OUT) or the expected DATA (IN).
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
        let ctrl = Control::OWNER::SET
            + Control::ENABLED::SET
            + Control::IN_USE::SET
            + Control::LENGTH.val(bytes.len() as u32)
            + Control::OUT.val(u32::from(token != IN));
        let mut host_errors = 0;
        let mut attempt = 0;
        while attempt < attempts {
            // DONE is W1C: clear a stale completion before asserting GO.
            r.control.write(ctrl + Control::DONE::SET);
            r.control.write(ctrl + Control::GO::SET);
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
            r.control.write(ctrl + Control::DONE::SET);
            if observed.is_set(Control::ERROR) {
                // A corrupted microframe is retried a few times; a hardware
                // error (port suspended or reset) is final.
                if observed.matches_all(Control::EXCEPTION::Transaction) {
                    host_errors += 1;
                    if host_errors < HOST_RETRIES {
                        continue;
                    }
                }
                return Err(ServiceError::IoError);
            }
            let reply = r.pids.read(Pids::RECEIVED) as u8;
            if accepted(token, *pid, reply) {
                let length = observed.read(Control::LENGTH) as usize;
                if length > bytes.len() || (token != IN && length != bytes.len()) {
                    return Err(ServiceError::IoError);
                }
                if token == IN {
                    unpack([r.data[0].get(), r.data[1].get()], &mut bytes[..length]);
                }
                *pid ^= DATA0 ^ DATA1;
                return Ok(Some(length));
            }
            // NAK, NYET or a repeated (out-of-sync) IN packet: the device is
            // busy; reissue. Anything else, a STALL included, is final.
            if !matches!(reply, NAK | NYET) && !(token == IN && reply == (*pid ^ (DATA0 ^ DATA1))) {
                return Err(ServiceError::IoError);
            }
            host_errors = 0;
            attempt += 1;
            delay(10);
        }
        Ok(None)
    }

    /// A control transfer on endpoint 0 (coreboot `dbgp_control_msg`).
    fn control(
        &self,
        device: u8,
        request_type: u8,
        request: u8,
        value: u16,
        index: u16,
        data: &mut [u8],
    ) -> Result<usize, ServiceError> {
        let setup = SetupPacket {
            request_type,
            request,
            value: value.into(),
            index: index.into(),
            length: (data.len() as u16).into(),
        };
        let mut bytes = [0; 8];
        bytes.copy_from_slice(setup.as_bytes());
        let mut pid = DATA0;
        if self.packet(device, 0, SETUP, &mut pid, &mut bytes, CONTROL_RETRIES)? != Some(8) {
            return Err(ServiceError::Timeout);
        }
        let input = request_type & FROM_DEVICE != 0;
        let length = if data.is_empty() {
            0
        } else {
            self.packet(
                device,
                0,
                if input { IN } else { OUT },
                &mut pid,
                data,
                CONTROL_RETRIES,
            )?
            .ok_or(ServiceError::Timeout)?
        };
        let mut status_pid = DATA1;
        if self.packet(
            device,
            0,
            if input { OUT } else { IN },
            &mut status_pid,
            &mut [],
            CONTROL_RETRIES,
        )? != Some(0)
        {
            return Err(ServiceError::IoError);
        }
        Ok(length)
    }

    /// Full gadget bring-up after a port reset (coreboot
    /// `probe_for_debug_descriptor`).
    fn enumerate(&mut self) -> Result<(), ServiceError> {
        let (input, output) = debug_endpoints(|device, ty, request, value, data| {
            self.control(device, ty, request, value, 0, data)
        })?;
        self.input = input;
        self.output = output;
        let mut pid = DATA0;
        let mut greeting = *b"USB\r\n";
        // FX2 gadgets need SET_CONFIGURATION before bulk endpoints work.
        if self
            .packet(
                DEVICE,
                self.output,
                OUT,
                &mut pid,
                &mut greeting,
                CONTROL_RETRIES,
            )
            .ok()
            .flatten()
            != Some(5)
        {
            self.control(DEVICE, TO_DEVICE, SET_CONFIGURATION, 1, 0, &mut [])?;
            pid = DATA0;
            if self.packet(
                DEVICE,
                self.output,
                OUT,
                &mut pid,
                &mut greeting,
                CONTROL_RETRIES,
            )? != Some(5)
            {
                return Err(ServiceError::Timeout);
            }
        }
        self.out_pid = pid;
        self.in_pid = DATA0;
        self.online = true;
        Ok(())
    }

    /// Continue with a gadget an earlier stage enumerated: read its
    /// endpoints back and reset both bulk toggles to DATA0.
    fn reattach(&mut self) -> Result<(), ServiceError> {
        let mut desc = [0; 4];
        let length = self.control(
            DEVICE,
            FROM_DEVICE,
            GET_DESCRIPTOR,
            DEBUG_DESCRIPTOR,
            0,
            &mut desc,
        )?;
        let (input, output) = parse_debug_descriptor(&desc[..length])?;
        // USB 2.0 9.4.5: CLEAR_FEATURE(ENDPOINT_HALT) reinitializes the
        // endpoint's data toggle to DATA0.
        for endpoint in [u16::from(output), 0x80 | u16::from(input)] {
            self.control(
                DEVICE,
                TO_ENDPOINT,
                CLEAR_FEATURE,
                ENDPOINT_HALT,
                endpoint,
                &mut [],
            )?;
        }
        self.input = input;
        self.output = output;
        self.out_pid = DATA0;
        self.in_pid = DATA0;
        self.online = true;
        Ok(())
    }

    /// Send the buffered output as one packet. A packet the host does not
    /// take is dropped, like coreboot's.
    fn send(&mut self) -> Result<(), ServiceError> {
        let length = core::mem::take(&mut self.tx_len);
        if length == 0 {
            return Ok(());
        }
        let mut data = self.tx;
        let mut pid = self.out_pid;
        match self.packet(
            DEVICE,
            self.output,
            OUT,
            &mut pid,
            &mut data[..length],
            self.out_retries,
        ) {
            Ok(Some(_)) => {
                self.out_pid = pid;
                self.out_retries = BULK_RETRIES;
                Ok(())
            }
            Ok(None) => {
                self.out_retries = BULK_RETRIES_STALLED;
                Err(ServiceError::Timeout)
            }
            Err(error) => {
                self.online = false;
                Err(error)
            }
        }
    }
}

/// Find the debug device at address 0 or, when it kept its address over the
/// port reset, at 127; move it to 127 and enter debug mode. Returns the IN
/// and OUT endpoint numbers.
fn debug_endpoints(
    mut control: impl FnMut(u8, u8, u8, u16, &mut [u8]) -> Result<usize, ServiceError>,
) -> Result<(u8, u8), ServiceError> {
    let mut desc = [0; 4];
    let (device, length) =
        match control(0, FROM_DEVICE, GET_DESCRIPTOR, DEBUG_DESCRIPTOR, &mut desc) {
            Ok(length) => (0, length),
            Err(_) => (
                DEVICE,
                control(
                    DEVICE,
                    FROM_DEVICE,
                    GET_DESCRIPTOR,
                    DEBUG_DESCRIPTOR,
                    &mut desc,
                )?,
            ),
        };
    let endpoints = parse_debug_descriptor(&desc[..length])?;
    if device == 0 {
        control(0, TO_DEVICE, SET_ADDRESS, DEVICE.into(), &mut [])?;
        // USB 2.0 9.2.6.3: SetAddress recovery.
        fstart_arch::udelay(2000);
    }
    control(DEVICE, TO_DEVICE, SET_FEATURE, DEVICE_DEBUG_MODE, &mut [])?;
    Ok(endpoints)
}

/// The debug descriptor's IN and OUT endpoint numbers.
fn parse_debug_descriptor(desc: &[u8]) -> Result<(u8, u8), ServiceError> {
    match *desc {
        [4, 10, input, output]
            if input & 0x0f != 0 && output != 0 && input & 0x70 == 0 && output & 0xf0 == 0 =>
        {
            Ok((input & 0x0f, output))
        }
        _ => Err(ServiceError::NotSupported),
    }
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

    /// The EHCI function (coreboot `ehci_debug_hw_enable`) and the offset of
    /// its debug port in BAR0.
    fn function(&self) -> Result<(ecam::EcamDevice, usize), ServiceError> {
        if ecam::base() == 0 {
            ecam::init(self.config.ecam_base);
        }
        if ecam::base() != self.config.ecam_base {
            return Err(ServiceError::InvalidParam);
        }
        let dev = ecam::EcamDevice::new(self.config.bus, self.config.device, self.config.function);
        if !dev.is_present() || dev.read32(8) >> 8 != EHCI_CLASS {
            return Err(ServiceError::NoDevice);
        }
        let cap = LocalRegisterCopy::<u32, DebugCapability::Register>::new(
            dev.read32(
                dev.find_capability(CAP_ID_DEBUG_PORT)
                    .ok_or(ServiceError::NotSupported)?,
            ),
        );
        // The operational registers and debug port must both be in BAR0.
        let offset = debug_port_offset(cap);
        if cap.read(DebugCapability::BAR) != 1 || offset < 0x10 || offset + 0x14 > BAR_SIZE as usize
        {
            return Err(ServiceError::NotSupported);
        }
        Ok((dev, offset))
    }

    /// BAR0 decodes at the configured address.
    fn decoding(&self, dev: ecam::EcamDevice) -> bool {
        // SAFETY: the EHCI function's standard type 0 header in ECAM.
        let header = unsafe { dev.regs::<PciType0Config>() };
        header.bar[0].get() & !0xf == self.config.bar
            && header.command.matches_all(
                PCI_COMMAND_BITS::MEMORY_SPACE::SET + PCI_COMMAND_BITS::BUS_MASTER::SET,
            )
    }

    /// Program BAR0 and bring the function to D0, decoding and mastering.
    fn enable(&self, dev: ecam::EcamDevice) {
        if let Some(pm) = dev.find_capability(CAP_ID_PM) {
            let mut control =
                LocalRegisterCopy::<u16, PowerControl::Register>::new(dev.read16(pm + 4));
            control.modify(PowerControl::STATE::D0 + PowerControl::PME_ENABLE::CLEAR);
            dev.write16(pm + 4, control.get());
        }
        // SAFETY: the EHCI function's standard type 0 header in ECAM.
        let header = unsafe { dev.regs::<PciType0Config>() };
        header.command.modify(PCI_COMMAND_BITS::MEMORY_SPACE::CLEAR);
        header.bar[0].set(self.config.bar);
        header
            .command
            .modify(PCI_COMMAND_BITS::MEMORY_SPACE::SET + PCI_COMMAND_BITS::BUS_MASTER::SET);
    }

    /// The register blocks of the decoding BAR0, checked for a sane layout.
    fn controller(&self, offset: usize) -> Result<Controller, ServiceError> {
        let base = self.config.bar as usize;
        // SAFETY: BAR0 decodes the platform-reserved debug window.
        let caps = unsafe { &*(base as *const Caps) };
        let length = usize::from(caps.length.get());
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
        Ok(Controller {
            caps,
            // SAFETY: inside BAR0, below the debug port (checked above).
            op: unsafe { &*((base + length) as *const Operational) },
            debug: base + offset,
        })
    }

    /// An earlier stage left the debug port claimed, the controller running
    /// and the gadget enabled on its port.
    fn running(controller: &Controller) -> bool {
        // SAFETY: the debug port registers inside the decoding BAR0.
        let control = &unsafe { &*(controller.debug as *const DebugPort) }.control;
        let port = controller.port();
        !controller.op.status.is_set(Status::HALTED)
            && controller.op.configured.is_set(Configured::CF)
            && port.matches_all(Port::CONNECTED::SET + Port::ENABLED::SET + Port::RESET::CLEAR)
            && control
                .matches_all(Control::OWNER::SET + Control::ENABLED::SET + Control::IN_USE::SET)
    }

    /// Reset the controller and the debug port and enumerate the gadget
    /// (coreboot `usbdebug_init_`).
    fn bring_up(&mut self, controller: &Controller) -> Result<(), ServiceError> {
        let op = controller.op;
        let s = self.state.get_mut();
        s.debug = controller.debug;
        op.command.modify(Command::RUN::CLEAR);
        wait(|| op.status.is_set(Status::HALTED), 100)?;
        op.command.modify(Command::RESET::SET);
        wait(|| !op.command.is_set(Command::RESET), 100)?;
        // Claim the debug port, but do not enable it yet.
        s.regs()
            .control
            .modify(Control::OWNER::SET + Control::ENABLED::CLEAR + Control::IN_USE::CLEAR);
        op.interrupts.set(0);
        // Run without either schedule: only debug-port transactions happen.
        // The rest of USBCMD (the interrupt threshold) keeps its reset value.
        op.command.modify(
            Command::RUN::SET
                + Command::PERIODIC::CLEAR
                + Command::ASYNC::CLEAR
                + Command::ASYNC_DOORBELL::CLEAR
                + Command::LIGHT_RESET::CLEAR,
        );
        op.configured.write(Configured::CF::SET);
        wait(|| !op.status.is_set(Status::HALTED), 100)?;
        let port = controller.port();
        // Port bits that are write-1-to-clear must be written as zero.
        let keep = |mut value: LocalRegisterCopy<u32, Port::Register>| {
            value.modify(
                Port::CONNECT_CHANGE::CLEAR
                    + Port::ENABLE_CHANGE::CLEAR
                    + Port::OVERCURRENT_CHANGE::CLEAR,
            );
            value
        };
        if controller.caps.structural.is_set(Structural::PPC) {
            let mut value = keep(port.extract());
            value.modify(Port::POWER::SET);
            port.set(value.get());
        }
        wait(|| port.is_set(Port::CONNECTED), 200)?;
        fstart_arch::udelay(100_000); // connection debounce
        // coreboot ehci_reset_port(): 50 ms root port reset, enable cleared.
        let mut value = keep(port.extract());
        value.modify(Port::ENABLED::CLEAR + Port::RESET::SET);
        port.set(value.get());
        fstart_arch::udelay(50_000);
        let mut value = keep(port.extract());
        value.modify(Port::RESET::CLEAR);
        port.set(value.get());
        wait(|| !port.is_set(Port::RESET), 100)?;
        if !port.is_set(Port::CONNECTED) || !port.is_set(Port::ENABLED) {
            return Err(ServiceError::NoDevice); // not a directly attached HS device
        }
        let claim = Control::OWNER::SET + Control::ENABLED::SET + Control::IN_USE::SET;
        s.regs().control.modify(claim);
        if !s.regs().control.matches_all(claim) {
            return Err(ServiceError::HardwareError);
        }
        fstart_arch::udelay(100_000);
        s.enumerate()
    }

    fn initialize(&mut self) -> Result<(), ServiceError> {
        let (dev, offset) = self.function()?;
        if self.decoding(dev) {
            let controller = self.controller(offset)?;
            if Self::running(&controller) {
                let s = self.state.get_mut();
                s.debug = controller.debug;
                if s.reattach().is_ok() {
                    return Ok(());
                }
                *s = State::new();
            }
        }
        self.enable(dev);
        let controller = self.controller(offset)?;
        self.bring_up(&controller)
    }
}

/// Offset of the debug port registers in the capability's BAR.
fn debug_port_offset(cap: LocalRegisterCopy<u32, DebugCapability::Register>) -> usize {
    cap.read(DebugCapability::OFFSET) as usize & !3
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
                // Give the debug port back.
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
            let mut result = Ok(());
            for &byte in bytes {
                s.tx[s.tx_len] = byte;
                s.tx_len += 1;
                if s.tx_len == s.tx.len() {
                    result = result.and(s.send());
                    if !s.online {
                        return result;
                    }
                }
            }
            result
        })
    }
    fn flush(&self) -> Result<(), ServiceError> {
        self.with_state(|s| if s.online { s.send() } else { Ok(()) })
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
    fn absent_controller_leaves_the_console_offline() {
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
        assert!(console.flush().is_ok());
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
                        (FROM_DEVICE, GET_DESCRIPTOR, DEBUG_DESCRIPTOR) => {
                            descriptors += 1;
                            if retained && device == 0 {
                                return Err(ServiceError::Timeout);
                            }
                            assert_eq!(device, if retained { DEVICE } else { 0 });
                            data.copy_from_slice(&[4, 10, 0x81, 2]);
                            Ok(4)
                        }
                        (TO_DEVICE, SET_ADDRESS, 127) => {
                            assert!(!retained);
                            assignments += 1;
                            Ok(0)
                        }
                        (TO_DEVICE, SET_FEATURE, DEVICE_DEBUG_MODE) => {
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
    fn debug_descriptor_needs_both_endpoints() {
        assert_eq!(parse_debug_descriptor(&[4, 10, 0x81, 2]), Ok((1, 2)));
        assert_eq!(parse_debug_descriptor(&[4, 10, 1, 1]), Ok((1, 1)));
        for bad in [
            &[4, 10, 0, 2][..],
            &[4, 10, 1, 0],
            &[4, 10, 0x91, 2],
            &[4, 10, 1, 0x12],
            &[4, 4, 1, 2],
            &[4, 10, 1],
        ] {
            assert_eq!(parse_debug_descriptor(bad), Err(ServiceError::NotSupported));
        }
    }

    #[test]
    fn debug_registers_and_setup_packet_match_spec() {
        assert_eq!(core::mem::offset_of!(DebugPort, address), 0x10);
        assert_eq!(core::mem::offset_of!(Operational, configured), 0x40);
        assert_eq!(core::mem::offset_of!(Operational, ports), 0x44);
        let setup = SetupPacket {
            request_type: FROM_DEVICE,
            request: GET_DESCRIPTOR,
            value: DEBUG_DESCRIPTOR.into(),
            index: 0.into(),
            length: 4.into(),
        };
        assert_eq!(setup.as_bytes(), &[0x80, 6, 0, 10, 0, 0, 4, 0]);
        // ICH9 EHCI: debug port at BAR0 + 0xa0.
        let cap = LocalRegisterCopy::<u32, DebugCapability::Register>::new(0x20a0_580a);
        assert_eq!(cap.read(DebugCapability::ID), u32::from(CAP_ID_DEBUG_PORT));
        assert_eq!(cap.read(DebugCapability::BAR), 1);
        assert_eq!(debug_port_offset(cap), 0xa0);
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
