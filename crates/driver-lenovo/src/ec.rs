//! Low-level ACPI EC handshake with explicit ports for SMM/OS ownership.
//!
//! Legacy firmware uses 0x62/0x66. H8 exposes a second channel at
//! 0x1600/0x1604 so SMM need not race the OS on the legacy channel.

use fstart_core::pio::{inb, outb};

const EC_OBF: u8 = 0x01;
const EC_IBF: u8 = 0x02;
const EC_SCI_EVT: u8 = 0x20;
const RD_EC: u8 = 0x80;
const WR_EC: u8 = 0x81;
const QR_EC: u8 = 0x84;
const TIMEOUT_US: u32 = 10_000;

/// A decoded EC command/data channel. Callers serialize transactions.
#[derive(Debug, Clone, Copy)]
pub struct Ec {
    data: u16,
    status: u16,
}

impl Ec {
    pub const LEGACY: Self = Self::new(0x62, 0x66);
    pub const H8_SMM: Self = Self::new(0x1600, 0x1604);

    pub const fn new(data: u16, status: u16) -> Self {
        Self { data, status }
    }

    #[inline(always)]
    pub fn status(&self) -> u8 {
        // SAFETY: caller selected a decoded EC channel.
        unsafe { inb(self.status) }
    }

    #[inline(always)]
    fn wait(&self, mask: u8, value: u8) -> bool {
        for _ in 0..TIMEOUT_US {
            if self.status() & mask == value {
                return true;
            }
            fstart_arch::udelay(1);
        }
        false
    }

    #[inline(always)]
    fn command(&self, command: u8) -> bool {
        if !self.wait(EC_IBF, 0) {
            return false;
        }
        // SAFETY: decoded EC command port.
        unsafe { outb(self.status, command) };
        true
    }

    fn send(&self, data: u8) -> bool {
        if !self.wait(EC_IBF, 0) {
            return false;
        }
        // SAFETY: decoded EC data port.
        unsafe { outb(self.data, data) };
        true
    }

    #[inline(always)]
    fn receive(&self) -> Option<u8> {
        if !self.wait(EC_OBF, EC_OBF) {
            return None;
        }
        // SAFETY: decoded EC data port.
        Some(unsafe { inb(self.data) })
    }

    pub fn read(&self, addr: u8) -> Option<u8> {
        if !self.command(RD_EC) || !self.send(addr) {
            return None;
        }
        self.receive()
    }

    pub fn write(&self, addr: u8, data: u8) -> bool {
        self.command(WR_EC) && self.send(addr) && self.send(data)
    }

    pub fn set_bit(&self, addr: u8, bit: u8) -> bool {
        self.read(addr)
            .is_some_and(|val| self.write(addr, val | (1 << bit)))
    }

    pub fn clear_bit(&self, addr: u8, bit: u8) -> bool {
        self.read(addr)
            .is_some_and(|val| self.write(addr, val & !(1 << bit)))
    }

    /// Query only if attention is asserted; bounded handshake on all paths.
    // Inline to avoid a promoted port-pair reference in the raw-copy SMM image.
    #[inline(always)]
    pub fn query_event(&self) -> Option<u8> {
        if self.status() & EC_SCI_EVT == 0 || !self.command(QR_EC) {
            return None;
        }
        self.receive()
    }

    pub fn clear_out_queue(&self) {
        for _ in 0..TIMEOUT_US {
            if self.status() & EC_OBF == 0 {
                return;
            }
            // SAFETY: drain the selected decoded EC channel.
            unsafe { inb(self.data) };
            fstart_arch::udelay(1);
        }
    }
}

pub fn ec_read(addr: u8) -> Option<u8> {
    Ec::LEGACY.read(addr)
}

pub fn ec_write(addr: u8, data: u8) -> bool {
    Ec::LEGACY.write(addr, data)
}

pub fn ec_set_bit(addr: u8, bit: u8) -> bool {
    Ec::LEGACY.set_bit(addr, bit)
}

pub fn ec_clr_bit(addr: u8, bit: u8) -> bool {
    Ec::LEGACY.clear_bit(addr, bit)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn h8_smm_channel_is_distinct_from_os_channel() {
        assert_eq!((Ec::LEGACY.data, Ec::LEGACY.status), (0x62, 0x66));
        assert_eq!((Ec::H8_SMM.data, Ec::H8_SMM.status), (0x1600, 0x1604));
    }
}
