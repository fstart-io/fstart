//! Low-level EC port-I/O handshake (data 0x62, command/status 0x66).
//!
//! Ported from coreboot `ec/acpi/ec.c`. All timeouts are simple spin loops
//! over `udelay`, matching firmware constraints (no timers up yet in the
//! hooks that talk to the EC).

use fstart_core::pio::{inb, outb};
use fstart_log::{Hex, info};

const EC_DATA: u16 = 0x62;
const EC_SC: u16 = 0x66;

/// Status register: output buffer full.
const EC_OBF: u8 = 0x01;
/// Status register: input buffer full.
const EC_IBF: u8 = 0x02;

const RD_EC: u8 = 0x80;
const WR_EC: u8 = 0x81;

const SEND_TIMEOUT_US: u32 = 10_000;
const RECV_TIMEOUT_US: u32 = 10_000;
const POLL_DELAY_US: u32 = 1;

fn wait_ec_sc(mask: u8, value: u8, timeout_us: u32) -> bool {
    let mut timeout = timeout_us;
    while timeout > 0 {
        // SAFETY: fixed EC status port decoded by the southbridge LPC.
        let sc = unsafe { inb(EC_SC) };
        if (sc & mask) == value {
            return true;
        }
        #[cfg(feature = "x86_64")]
        fstart_arch::x86::udelay(POLL_DELAY_US);
        #[cfg(not(feature = "x86_64"))]
        for _ in 0..200 {
            core::hint::spin_loop();
        }
        timeout = timeout.saturating_sub(POLL_DELAY_US);
    }
    false
}

fn send_ec_command(command: u8) -> bool {
    if !wait_ec_sc(EC_IBF, 0, SEND_TIMEOUT_US) {
        info!(
            "lenovo-ec: timeout sending command {}",
            Hex(u64::from(command))
        );
        return false;
    }
    // SAFETY: fixed EC command port decoded by the southbridge LPC.
    unsafe { outb(EC_SC, command) };
    true
}

fn send_ec_data(data: u8) -> bool {
    if !wait_ec_sc(EC_IBF, 0, SEND_TIMEOUT_US) {
        info!("lenovo-ec: timeout sending data {}", Hex(u64::from(data)));
        return false;
    }
    // SAFETY: fixed EC data port decoded by the southbridge LPC.
    unsafe { outb(EC_DATA, data) };
    true
}

fn recv_ec_data() -> Option<u8> {
    if !wait_ec_sc(EC_OBF, EC_OBF, RECV_TIMEOUT_US) {
        info!("lenovo-ec: timeout receiving data");
        return None;
    }
    // SAFETY: fixed EC data port decoded by the southbridge LPC.
    Some(unsafe { inb(EC_DATA) })
}

/// Read one byte of EC RAM at `addr`.
pub fn ec_read(addr: u8) -> Option<u8> {
    if !send_ec_command(RD_EC) || !send_ec_data(addr) {
        return None;
    }
    recv_ec_data()
}

/// Write one byte of EC RAM at `addr`.
pub fn ec_write(addr: u8, data: u8) -> bool {
    send_ec_command(WR_EC) && send_ec_data(addr) && send_ec_data(data)
}

/// Set a single bit in EC RAM.
pub fn ec_set_bit(addr: u8, bit: u8) -> bool {
    let Some(val) = ec_read(addr) else {
        return false;
    };
    ec_write(addr, val | (1 << bit))
}

/// Clear a single bit in EC RAM.
pub fn ec_clr_bit(addr: u8, bit: u8) -> bool {
    let Some(val) = ec_read(addr) else {
        return false;
    };
    ec_write(addr, val & !(1 << bit))
}
