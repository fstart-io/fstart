//! ACPI EC command/data handshake on a board-supplied channel.
//!
//! The transport has no Lenovo-specific addresses. Callers serialize the
//! selected channel; timed-out transactions remain explicitly fallible.

use fstart_core::pio::PioRegister;
use fstart_core::typed::{Io8, IoAddr};
use tock_registers::fields::FieldValue;
use tock_registers::interfaces::{Readable, Writeable};
use tock_registers::{LocalRegisterCopy, RegisterLongName, register_bitfields};

/// Bind a controller's field namespace to its EC RAM byte index.
/// This is distinct from the command/status port and PMH7 register namespaces.
pub trait EcRegister: RegisterLongName {
    const INDEX: u8;
}

register_bitfields![u8,
    pub EC_STATUS [
        OUTPUT_FULL OFFSET(0) NUMBITS(1) [],
        INPUT_FULL OFFSET(1) NUMBITS(1) [],
        COMMAND OFFSET(3) NUMBITS(1) [],
        BURST OFFSET(4) NUMBITS(1) [],
        SCI_EVENT OFFSET(5) NUMBITS(1) [],
        SMI_EVENT OFFSET(6) NUMBITS(1) []
    ]
];

/// ACPI-defined EC command encodings, including optional burst mode.
#[derive(Clone, Copy)]
#[repr(u8)]
pub enum EcCommand {
    Read = 0x80,
    Write = 0x81,
    BurstEnable = 0x82,
    BurstDisable = 0x83,
    Query = 0x84,
}

const TIMEOUT_US: u32 = 10_000;

/// Command/data port wiring shared by runtime and ACPI resource emission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EcPorts {
    pub data: IoAddr<Io8>,
    pub command: IoAddr<Io8>,
}

impl EcPorts {
    pub const fn new(data: u16, command: u16) -> Self {
        Self {
            data: IoAddr::new(data),
            command: IoAddr::new(command),
        }
    }
}

/// A decoded EC channel. Callers own transaction serialization.
#[derive(Debug, Clone, Copy)]
pub struct Ec {
    ports: EcPorts,
}

impl Ec {
    pub const fn new(ports: EcPorts) -> Self {
        Self { ports }
    }

    #[inline(always)]
    fn status_register(&self) -> PioRegister<u8, EC_STATUS::Register> {
        PioRegister::new(self.ports.command.raw())
    }

    #[inline(always)]
    fn data_register(&self) -> PioRegister<u8> {
        PioRegister::new(self.ports.data.raw())
    }

    #[inline(always)]
    pub fn status(&self) -> u8 {
        self.status_register().get()
    }

    #[inline(always)]
    fn wait(&self, expected: FieldValue<u8, EC_STATUS::Register>) -> bool {
        for _ in 0..TIMEOUT_US {
            if self.status_register().matches_all(expected) {
                return true;
            }
            fstart_arch::udelay(1);
        }
        false
    }

    #[inline(always)]
    fn command(&self, command: EcCommand) -> bool {
        if !self.wait(EC_STATUS::INPUT_FULL::CLEAR) {
            return false;
        }
        // Command and status share a port; commands are exact writes, not RMW.
        self.status_register().set(command as u8);
        true
    }

    fn send(&self, data: u8) -> bool {
        if !self.wait(EC_STATUS::INPUT_FULL::CLEAR) {
            return false;
        }
        self.data_register().set(data);
        true
    }

    #[inline(always)]
    fn receive(&self) -> Option<u8> {
        if !self.wait(EC_STATUS::OUTPUT_FULL::SET) {
            return None;
        }
        Some(self.data_register().get())
    }

    pub fn read(&self, addr: u8) -> Option<u8> {
        if !self.command(EcCommand::Read) || !self.send(addr) {
            return None;
        }
        self.receive()
    }

    pub fn write(&self, addr: u8, data: u8) -> bool {
        self.command(EcCommand::Write) && self.send(addr) && self.send(data)
    }

    pub fn read_register<R: EcRegister>(&self) -> Option<LocalRegisterCopy<u8, R>> {
        self.read(R::INDEX).map(LocalRegisterCopy::new)
    }

    /// Exact byte write; callers supply a full register value, including when
    /// issuing commands that must never read the register first.
    pub fn write_register<R: EcRegister>(&self, value: u8) -> bool {
        self.write(R::INDEX, value)
    }

    /// Modify a field's own register, preserving unrelated bits and reporting
    /// read or write handshake failure without fabricating a snapshot.
    ///
    /// Port status fields cannot be used as EC RAM fields:
    /// ```compile_fail
    /// use fstart_driver_lenovo::ec::{Ec, EC_STATUS};
    /// fn wrong_address_space(ec: &Ec) {
    ///     ec.modify_register(EC_STATUS::INPUT_FULL::SET);
    /// }
    /// ```
    pub fn modify_register<R: EcRegister>(&self, fields: FieldValue<u8, R>) -> bool {
        let Some(mut value) = self.read_register::<R>() else {
            return false;
        };
        value.modify(fields);
        self.write_register::<R>(value.get())
    }

    pub fn set_bit(&self, addr: u8, bit: u8) -> bool {
        bit < 8
            && self
                .read(addr)
                .is_some_and(|val| self.write(addr, val | (1 << bit)))
    }

    pub fn clear_bit(&self, addr: u8, bit: u8) -> bool {
        bit < 8
            && self
                .read(addr)
                .is_some_and(|val| self.write(addr, val & !(1 << bit)))
    }

    /// Query only if attention is asserted; bounded handshake on all paths.
    // Inline to avoid promoted port-pair references in the raw-copy SMM image.
    #[inline(always)]
    pub fn query_event(&self) -> Option<u8> {
        if !self.status_register().is_set(EC_STATUS::SCI_EVENT) || !self.command(EcCommand::Query) {
            return None;
        }
        self.receive()
    }

    pub fn clear_out_queue(&self) {
        for _ in 0..TIMEOUT_US {
            if !self.status_register().is_set(EC_STATUS::OUTPUT_FULL) {
                return;
            }
            let _ = self.data_register().get();
            fstart_arch::udelay(1);
        }
    }
}
