//! ACPI Embedded Controller command transport.
//!
//! This is the reusable EC protocol from coreboot `ec/acpi/ec.c`: a command
//! port, a data port, status polling, and RD/WR/query helpers. Lenovo H8 and
//! other laptop EC drivers build on this rather than open-coding port I/O.

#![no_std]

use fstart_services::ServiceError;
use serde::{Deserialize, Serialize};

const EC_POLL_DELAY_US: u32 = 10;
const EC_SEND_TIMEOUT_US: u32 = 20_000;
const EC_RECV_TIMEOUT_US: u32 = 320_000;
const EC_IBF: u8 = 1 << 1;
const EC_OBF: u8 = 1 << 0;
const RD_EC: u8 = 0x80;
const WR_EC: u8 = 0x81;
const QR_EC: u8 = 0x84;

/// ACPI EC I/O port pair.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct EcPorts {
    /// EC status/command port.
    pub cmd: u16,
    /// EC data port.
    pub data: u16,
}

impl EcPorts {
    /// Standard ACPI EC ports used by ThinkPad H8 in normal firmware stages.
    pub const STANDARD: Self = Self {
        cmd: 0x66,
        data: 0x62,
    };
    /// Alternate EC ports used by coreboot/fstart SMM-compatible handlers.
    pub const ALT: Self = Self {
        cmd: 0x1604,
        data: 0x1600,
    };
}

impl Default for EcPorts {
    fn default() -> Self {
        Self::STANDARD
    }
}

/// ACPI EC command transport.
#[derive(Debug, Clone, Copy)]
pub struct AcpiEc {
    ports: EcPorts,
}

impl AcpiEc {
    /// Create an EC transport for the supplied ports.
    pub const fn new(ports: EcPorts) -> Self {
        Self { ports }
    }

    /// Create an EC transport on standard ports 0x66/0x62.
    pub const fn standard() -> Self {
        Self::new(EcPorts::STANDARD)
    }

    /// Create an EC transport on alternate ports 0x1604/0x1600.
    pub const fn alternate() -> Self {
        Self::new(EcPorts::ALT)
    }

    /// Return configured ports.
    pub const fn ports(&self) -> EcPorts {
        self.ports
    }

    fn status(&self) -> u8 {
        // SAFETY: EC status port is a legacy x86 LPC I/O port decoded by the southbridge.
        unsafe { fstart_pio::inb(self.ports.cmd) }
    }

    fn wait_status(&self, timeout_us: u32, mask: u8, state: u8) -> Result<(), ServiceError> {
        let mut remaining = timeout_us;
        while remaining > 0 && (self.status() & mask) != state {
            fstart_arch_x86::udelay(EC_POLL_DELAY_US);
            remaining = remaining.saturating_sub(EC_POLL_DELAY_US);
        }
        if remaining == 0 {
            Err(ServiceError::Timeout)
        } else {
            Ok(())
        }
    }

    /// Wait until the EC input buffer is empty.
    pub fn ready_send(&self) -> Result<(), ServiceError> {
        self.wait_status(EC_SEND_TIMEOUT_US, EC_IBF, 0)
    }

    /// Wait until the EC output buffer is full.
    pub fn ready_recv(&self) -> Result<(), ServiceError> {
        self.wait_status(EC_RECV_TIMEOUT_US, EC_OBF, EC_OBF)
    }

    /// Send an EC command byte.
    pub fn send_command(&self, command: u8) -> Result<(), ServiceError> {
        self.ready_send()?;
        // SAFETY: command port is an EC I/O port and `ready_send` observed IBF clear.
        unsafe { fstart_pio::outb(self.ports.cmd, command) };
        Ok(())
    }

    /// Send an EC data byte.
    pub fn send_data(&self, data: u8) -> Result<(), ServiceError> {
        self.ready_send()?;
        // SAFETY: data port is an EC I/O port and `ready_send` observed IBF clear.
        unsafe { fstart_pio::outb(self.ports.data, data) };
        Ok(())
    }

    /// Receive one EC data byte.
    pub fn recv_data(&self) -> Result<u8, ServiceError> {
        self.ready_recv()?;
        // SAFETY: data port is an EC I/O port and `ready_recv` observed OBF set.
        Ok(unsafe { fstart_pio::inb(self.ports.data) })
    }

    /// Clear queued EC output bytes.
    pub fn clear_out_queue(&self) {
        let mut timeout = EC_RECV_TIMEOUT_US;
        while timeout > 0 && (self.status() & EC_OBF) != 0 {
            // SAFETY: discard pending byte from EC data port.
            let _ = unsafe { fstart_pio::inb(self.ports.data) };
            fstart_arch_x86::udelay(EC_POLL_DELAY_US);
            timeout = timeout.saturating_sub(EC_POLL_DELAY_US);
        }
    }

    /// Read an EC register using ACPI `RD_EC`.
    pub fn read(&self, addr: u8) -> Result<u8, ServiceError> {
        self.send_command(RD_EC)?;
        self.send_data(addr)?;
        self.recv_data()
    }

    /// Write an EC register using ACPI `WR_EC`.
    pub fn write(&self, addr: u8, data: u8) -> Result<(), ServiceError> {
        self.send_command(WR_EC)?;
        self.send_data(addr)?;
        self.send_data(data)
    }

    /// Read the pending EC query code using ACPI `QR_EC`.
    pub fn query(&self) -> Result<u8, ServiceError> {
        self.send_command(QR_EC)?;
        self.recv_data()
    }

    /// Set a bit in an EC register.
    pub fn set_bit(&self, addr: u8, bit: u8) -> Result<(), ServiceError> {
        self.write(addr, self.read(addr)? | (1 << bit))
    }

    /// Clear a bit in an EC register.
    pub fn clear_bit(&self, addr: u8, bit: u8) -> Result<(), ServiceError> {
        self.write(addr, self.read(addr)? & !(1 << bit))
    }
}
