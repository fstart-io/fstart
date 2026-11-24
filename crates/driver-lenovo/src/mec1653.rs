// SPDX-License-Identifier: MIT OR Apache-2.0
//! Microchip MEC1653 embedded controller early initialization and debug UART.
//!
//! Ported from coreboot `ec/lenovo/mec1653`. ThinkPads of the Skylake/Kaby Lake
//! generation (such as the T470s/T480/T580/X280) pair the H8 runtime interface
//! with a MEC1653 EC that requires early BIOS-alive signalling on debug port 1
//! (0x86), EC1 interface enablement, and LED power-on reset in the bootblock.

use crate::ec::{Ec, EcPorts};
use crate::h8::{CONFIG0, H8, H8Led, H8LedMode};
use fstart_core::pio::{inb, inl, outb, outl};

/// BIOS Debug Port 1 used to signal early boot progress to the MEC1653.
pub const DEBUG_PORT1: u16 = 0x86;
/// MEC1653 Super I/O PnP configuration index/data port base (0x4e/0x4f).
pub const CFG_PORT: u16 = 0x4e;
/// Standard ACPI EC0 command/data ports (0x62/0x66).
pub const EC0_PORTS: EcPorts = EcPorts::new(0x0062, 0x0066);
/// MEC1653 EC3 debug dword command/data ports (0x1618/0x161c).
pub const EC3_PORTS: EcPorts = EcPorts::new(0x1618, 0x161c);

const MEC_CFG_ENTRY_KEY: u8 = 0x55;
const MEC_CFG_EXIT_KEY: u8 = 0xaa;
const PNP_LDN_SELECT: u8 = 0x07;
const LDN_UART: u8 = 0x07;
const LDN_LPCIF: u8 = 0x0c;
const LPCIF_SIRQ_BASE: u8 = 0x40;
const LPCIF_BAR_UART: u8 = 0x80;
const UART_ACTIVATE: u8 = 0x30;
const UART_CONFIG_SELECT: u8 = 0xf0;
const UART_PORT: u16 = 0x3f8;
const UART_IRQ: u8 = 4;

const EC_DEBUG_CMD: u8 = 0x3d;
const EC_DEBUG_KEY_BASE: u8 = 0x3e;
const DEBUG_RW_KEY_IDX: u8 = 1;
const MEC1653_CFG_REG: u32 = 0xf0c400;
const MEC1653_CFG_TX_GPIO_OFFSET: u32 = 0x110;
const MEC1653_CFG_RX_GPIO_OFFSET: u32 = 0x114;

/// Coreboot `bootblock_ec_init()` for the MEC1653:
/// 1. Tell the EC via BIOS Debug Port 1 (0x86) that BIOS code is running.
/// 2. Enable accesses to the EC1 (0x1600/0x1604) interface via `CONFIG0`.
/// 3. Reset the power, suspend, and lid-logo LEDs to power-on state so warm
///    reboots do not leave LEDs off.
pub fn early_init(ports: EcPorts) -> bool {
    // SAFETY: port 0x86 is the chipset-decoded BIOS Debug Port 1 wired to MEC1653.
    unsafe {
        outb(DEBUG_PORT1, 0x11);
        outb(DEBUG_PORT1, 0x6e);
    }

    let ec = Ec::new(ports);
    ec.clear_out_queue();
    // Enable EC1 (SMM/auxiliary H8 channel) without clobbering existing CONFIG0 bits
    // on warm reboot.
    let mut ok = ec.modify_register(CONFIG0::SMM_ENABLE::SET);

    let h8 = H8::new(ec);
    ok &= h8.set_led(H8Led::Power, H8LedMode::On);
    ok &= h8.set_led(H8Led::Suspend, H8LedMode::Off);
    ok &= h8.set_led(H8Led::Logo, H8LedMode::On);
    ok
}

/// Parse a 16-character hexadecimal MEC1653 debug unlock key into 8 bytes.
#[must_use]
pub const fn parse_unlock_key(hex: &str) -> Option<[u8; 8]> {
    let bytes = hex.as_bytes();
    if bytes.len() != 16 {
        return None;
    }
    let mut out = [0u8; 8];
    let mut i = 0;
    while i < 8 {
        let Some(hi) = hex_nibble(bytes[i * 2]) else {
            return None;
        };
        let Some(lo) = hex_nibble(bytes[i * 2 + 1]) else {
            return None;
        };
        out[i] = (hi << 4) | lo;
        i += 1;
    }
    Some(out)
}

const fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn pnp_write(reg: u8, val: u8) {
    // SAFETY: 0x4e/0x4f is decoded to the MEC1653 PnP interface by LPC_IOE.
    unsafe {
        outb(CFG_PORT, reg);
        outb(CFG_PORT + 1, val);
    }
}

fn pnp_write32(offset: u8, val: u32) {
    for (i, byte) in val.to_le_bytes().into_iter().enumerate() {
        pnp_write(offset + i as u8, byte);
    }
}

fn ec3_wait_send(timeout_us: u32) -> bool {
    for _ in 0..timeout_us {
        // SAFETY: EC3 status port 0x161c is inside the board's 0x1600..0x1680 decode.
        if unsafe { inb(EC3_PORTS.command.raw()) } & 0x02 == 0 {
            return true;
        }
        fstart_arch::udelay(1);
    }
    false
}

fn ec3_clear_out_queue(timeout_us: u32) {
    for _ in 0..timeout_us {
        // SAFETY: EC3 ports 0x1618/0x161c are inside the board's 0x1600..0x1680 decode.
        if unsafe { inb(EC3_PORTS.command.raw()) } & 0x01 == 0 {
            return;
        }
        let _ = unsafe { inl(EC3_PORTS.data.raw()) };
        fstart_arch::udelay(1);
    }
}

fn debug_write_dword(addr: u32, val: u32) -> bool {
    const TIMEOUT_US: u32 = 20_000;
    ec3_clear_out_queue(TIMEOUT_US);
    if !ec3_wait_send(TIMEOUT_US) {
        return false;
    }
    // SAFETY: EC3 data port 0x1618 accepts 32-bit debug write commands/values.
    unsafe { outl(EC3_PORTS.data.raw(), (addr << 8) | 0xea) };
    if !ec3_wait_send(TIMEOUT_US) {
        return false;
    }
    unsafe { outl(EC3_PORTS.data.raw(), val) };
    true
}

/// Enable the MEC1653 internal debug UART at COM1 (0x3f8 / IRQ4) and route its
/// TX/RX GPIOs using the board's RW debug unlock key (coreboot
/// `mec1653_configure_uart`).
pub fn configure_uart(ports: EcPorts, unlock_key: [u8; 8]) -> bool {
    // Enter PnP configuration mode.
    // SAFETY: 0x4e is decoded to the MEC1653 configuration port.
    unsafe { outb(CFG_PORT, MEC_CFG_ENTRY_KEY) };

    // Configure LPC interface UART BAR and SIRQ4 routing.
    pnp_write(PNP_LDN_SELECT, LDN_LPCIF);
    pnp_write32(LPCIF_BAR_UART, (u32::from(UART_PORT) << 16) | 0x8707);
    pnp_write(LPCIF_SIRQ_BASE + UART_IRQ, LDN_UART);

    // Enable UART LDN.
    pnp_write(PNP_LDN_SELECT, LDN_UART);
    pnp_write(UART_ACTIVATE, 1);
    pnp_write(UART_CONFIG_SELECT, 0);

    // Exit PnP configuration mode.
    unsafe { outb(CFG_PORT, MEC_CFG_EXIT_KEY) };

    // Supply RW debug unlock key on EC0 and wait for command completion.
    let ec = Ec::new(ports);
    for (j, byte) in unlock_key.into_iter().enumerate() {
        if !ec.write(EC_DEBUG_KEY_BASE + j as u8, byte) {
            return false;
        }
    }
    if !ec.write(EC_DEBUG_CMD, 0xc0 | (DEBUG_RW_KEY_IDX & 0x0f)) {
        return false;
    }
    let mut unlocked = false;
    for _ in 0..20_000 {
        match ec.read(EC_DEBUG_CMD) {
            Some(status) if status & 0x80 == 0 => {
                unlocked = true;
                break;
            }
            Some(_) => fstart_arch::udelay(1),
            None => return false,
        }
    }
    if !unlocked {
        return false;
    }

    // Route UART TX and RX pins inside the MEC1653 GPIO controller.
    debug_write_dword(MEC1653_CFG_REG + MEC1653_CFG_TX_GPIO_OFFSET, 0x1000)
        && debug_write_dword(MEC1653_CFG_REG + MEC1653_CFG_RX_GPIO_OFFSET, 0x1000)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_t480_unlock_key_and_reject_malformed_inputs() {
        assert_eq!(
            parse_unlock_key("7a41b149fe2101cf"),
            Some([0x7a, 0x41, 0xb1, 0x49, 0xfe, 0x21, 0x01, 0xcf])
        );
        assert_eq!(
            parse_unlock_key("7A41B149FE2101CF"),
            Some([0x7a, 0x41, 0xb1, 0x49, 0xfe, 0x21, 0x01, 0xcf])
        );
        assert_eq!(parse_unlock_key("7a41"), None);
        assert_eq!(parse_unlock_key("7a41b149fe2101cg"), None);
    }
}
