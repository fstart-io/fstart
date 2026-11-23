// SPDX-License-Identifier: MIT OR Apache-2.0
//! Intel P2SB sideband transactions (the IOSF > 1.0 SBI interface).
//!
//! Derived from coreboot common/block/pcr/pcr.c:pcr_execute_sideband_msg
//! and common/block/include/intelblocks/pcr.h. The SBREG/PCR MMIO transport
//! remains separate; this driver uses the existing typed PCI ECAM registers.

use embedded_hal::delay::DelayNs;
use fstart_core::mmio::MmioRegister;
use fstart_pci::ecam::EcamDevice;
use tock_registers::{
    interfaces::{ReadWriteable, Readable, Writeable},
    register_bitfields,
};

register_bitfields![u32,
    Address [
        OFFSET OFFSET(0) NUMBITS(16) [],
        PORT OFFSET(24) NUMBITS(8) []
    ]
];
register_bitfields![u16,
    Status [
        BUSY OFFSET(0) NUMBITS(1) [],
        RESPONSE OFFSET(1) NUMBITS(2) [],
        POSTED OFFSET(7) NUMBITS(1) [],
        OPCODE OFFSET(8) NUMBITS(8) []
    ],
    Route [
        FUNCTION OFFSET(0) NUMBITS(8) [],
        BAR OFFSET(8) NUMBITS(3) [],
        BYTE_ENABLE OFFSET(12) NUMBITS(4) []
    ]
];

const ADDRESS: u16 = 0xd0;
const DATA: u16 = 0xd4;
const STATUS: u16 = 0xd8;
const ROUTE: u16 = 0xda;
const EXTENDED_ADDRESS: u16 = 0xdc;
// Native uses a 10-ms stopwatch. This is a finite 10,000 one-us-delay budget,
// not a calibration or guarantee of elapsed wall time.
const POLLS: usize = 10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Opcode {
    MemoryRead = 0x00,
    MemoryWrite = 0x01,
    PciConfigRead = 0x04,
    PciConfigWrite = 0x05,
    PcrRead = 0x06,
    PcrWrite = 0x07,
    GpioLockUnlock = 0x13,
}
impl Opcode {
    fn reads(self) -> bool {
        matches!(self, Self::MemoryRead | Self::PciConfigRead | Self::PcrRead)
    }
}

/// Explicit source SBI fields; no implicit posted/default policy.
///
/// This bounded interface represents the u16-offset subset used by native
/// P2SB register accessors. It always clears the extended address. It does not
/// silently truncate a BAR/byte-enable or support extended memory addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Message {
    pub port: u8,
    pub offset: u16,
    pub opcode: Opcode,
    pub posted: bool,
    pub byte_enable: u8,
    pub bar: u8,
    pub function: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    InvalidMessage,
    Unavailable,
    PreviousBusyTimeout,
    CompletionTimeout,
    /// Native response: 1=unsupported, 2=powered down, 3=multicast mixed.
    Response(u8),
    /// A previously started transaction timed out; its effects are uncertain.
    Poisoned,
}

/// Exclusively owned SBI engine, not an ECAM accessor or P2SB visibility policy.
/// No automatic unhide, replay, cancellation, reset, or hardware rollback.
pub struct Sbi<'a> {
    device: &'a EcamDevice,
    poisoned: bool,
}
impl<'a> Sbi<'a> {
    /// # Safety
    /// `device` must actually be an Intel P2SB function with this SBI register
    /// layout, held mapped and accessible for the entire binding. Vendor ID
    /// alone does not prove its identity. Caller owns all target endpoints and
    /// registers accessed by messages, as well as the whole engine, and
    /// serializes CPUs/APs/SMM/other agents; no competing hide/lock is allowed.
    /// Rebinding after an uncertain transaction requires platform recovery or
    /// separately established completion, not an attempted automatic retry.
    pub unsafe fn new(device: &'a EcamDevice) -> Result<Self, Error> {
        if device.vendor_id() != 0x8086 {
            return Err(Error::Unavailable);
        }
        Ok(Self {
            device,
            poisoned: false,
        })
    }

    /// Execute one actual transaction. Reads update `data` only on successful
    /// response; writes and all errors leave the caller's value unchanged.
    /// A completed error response can still mean partial endpoint effects
    /// (especially multicast mixed). Posted success is not endpoint validation.
    pub fn execute(
        &mut self,
        msg: Message,
        data: &mut u32,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        if self.poisoned {
            return Err(Error::Poisoned);
        }
        if msg.port == 0xff || msg.byte_enable > 15 || msg.bar > 7 {
            return Err(Error::InvalidMessage);
        }
        if self.device.vendor_id() != 0x8086 {
            return Err(Error::Unavailable);
        }
        // SAFETY: new() binds the actual owned P2SB layout; widths/alignment
        // are the native exact dword/word accesses, all inside PCI config space.
        let (address, value, status, route, extended) = unsafe {
            (
                self.device.register::<u32, Address::Register>(ADDRESS),
                self.device.register::<u32, ()>(DATA),
                self.device.register::<u16, Status::Register>(STATUS),
                self.device.register::<u16, Route::Register>(ROUTE),
                self.device.register::<u32, ()>(EXTENDED_ADDRESS),
            )
        };
        if !wait_idle(&status, delay) {
            return Err(Error::PreviousBusyTimeout);
        }
        address.write(
            Address::PORT.val(u32::from(msg.port)) + Address::OFFSET.val(u32::from(msg.offset)),
        );
        extended.set(0);
        status.modify(
            Status::OPCODE.val(msg.opcode as u16) + Status::POSTED.val(u16::from(msg.posted)),
        );
        route.write(
            Route::BYTE_ENABLE.val(u16::from(msg.byte_enable))
                + Route::BAR.val(u16::from(msg.bar))
                + Route::FUNCTION.val(u16::from(msg.function)),
        );
        value.set(if msg.opcode.reads() { 0 } else { *data });
        status.modify(Status::BUSY::SET);
        if !wait_idle(&status, delay) {
            self.poisoned = true;
            return Err(Error::CompletionTimeout);
        }
        let response = status.read(Status::RESPONSE) as u8;
        if response != 0 {
            return Err(Error::Response(response));
        }
        if msg.opcode.reads() {
            *data = value.get();
        }
        Ok(())
    }
}

fn wait_idle(status: &MmioRegister<u16, Status::Register>, delay: &mut impl DelayNs) -> bool {
    for _ in 0..POLLS {
        if !status.is_set(Status::BUSY) {
            return true;
        }
        delay.delay_us(1);
    }
    false
}

#[cfg(test)]
#[path = "p2sb_tests.rs"]
mod tests;
