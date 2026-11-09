//! K3 SRAM bootblock: the boot ROM enters it in M-mode on hart 0 at
//! `0xC080_1000`, with UART0 clocked and its pins routed.

use fstart_driver_uart::ns16550::{AccessMode, Ns16550, Ns16550Config};
use fstart_log::{Hex, StaticConsole};

/// UART0, the console the boot ROM sets up.
pub const UART0_BASE: u64 = 0xD401_7000;
/// UART0 runs from the 14.7456 MHz clock the boot ROM selects for it.
pub const UART0_CLOCK: u32 = 14_745_600;
const UART_REG_SHIFT: u8 = 2;
/// IER bit 6, UART unit enable: this UART stays off while it is clear.
const UART_IER_UUE: u32 = 1 << 6;

/// Where the boot ROM records the source it loaded the FSBL from.
const BOOT_DEV_FLAG: usize = 0xD428_2D10;
const BOOT_DEV_MASK: u32 = 0xfff;
/// Boot strap latch; STRAP[3:0] sit at bits 12:9.
const BOOT_STRAPS: usize = 0xD428_2C20;
const BOOT_STRAP_SHIFT: u32 = 9;

/// UART0 at 115200 8N1.
pub const fn uart0_console() -> Ns16550Config {
    Ns16550Config {
        regs: AccessMode::Mmio {
            base: UART0_BASE,
            reg_shift: UART_REG_SHIFT,
            reg_width: 4,
        },
        clock_freq: UART0_CLOCK,
        baud_rate: 115_200,
    }
}

fn boot_source(flag: u32) -> &'static str {
    match flag & BOOT_DEV_MASK {
        0x55a => "USB fastboot",
        0x66b => "UART download",
        0xb00 => "eMMC",
        0xb01 => "SPI NOR",
        0xb02 => "SPI NAND",
        0xb03 => "UFS",
        0xb10 => "SD",
        _ => "unknown",
    }
}

/// Secondary boot device STRAP[1:0] selects behind the SD card.
fn strap_device(straps: u32) -> &'static str {
    match straps & 0b11 {
        0b00 => "eMMC",
        0b01 => "SPI NOR",
        0b10 => "SPI NAND",
        _ => "UFS",
    }
}

fn read32(addr: usize) -> u32 {
    // SAFETY: fixed, always-readable K3 system controller registers.
    unsafe { core::ptr::read_volatile(addr as *const u32) }
}

macro_rules! csr {
    ($csr:literal) => {{
        let value: u64;
        // SAFETY: reading a CSR M-mode can access has no side effects.
        unsafe { core::arch::asm!(concat!("csrr {}, ", $csr), out(reg) value) };
        value
    }};
}

/// Bring up the console and report the state the boot ROM left behind.
/// Boards have no work here; their bootblock `main` calls this.
pub fn bootblock(slot: &'static StaticConsole<Ns16550>, console: Ns16550Config) -> ! {
    let Ok(mut uart) = Ns16550::new(console) else {
        fstart_arch::halt()
    };
    if uart.init().is_err() {
        fstart_arch::halt()
    }
    // `init` clears IER, which also switches this UART off.
    if let AccessMode::Mmio {
        base, reg_shift, ..
    } = console.regs
    {
        let ier = (base + (1 << reg_shift)) as *mut u32;
        // SAFETY: IER of the console UART just initialised.
        unsafe { core::ptr::write_volatile(ier, UART_IER_UUE) };
    }
    slot.install(uart);

    let flag = read32(BOOT_DEV_FLAG);
    let straps = (read32(BOOT_STRAPS) >> BOOT_STRAP_SHIFT) & 0xf;
    fstart_log::info!("k3: bootblock on hart {}", csr!("mhartid"));
    fstart_log::info!(
        "boot source {} ({}), straps {}: SD then {}{}",
        boot_source(flag),
        Hex(flag.into()),
        Hex(straps.into()),
        strap_device(straps),
        if straps & 0b1000 != 0 {
            ", download"
        } else {
            ""
        }
    );
    fstart_log::info!(
        "mvendorid {} marchid {} mimpid {} misa {}",
        Hex(csr!("mvendorid")),
        Hex(csr!("marchid")),
        Hex(csr!("mimpid")),
        Hex(csr!("misa"))
    );
    fstart_log::info!(
        "mstatus {} mtvec {} mie {} mip {}",
        Hex(csr!("mstatus")),
        Hex(csr!("mtvec")),
        Hex(csr!("mie")),
        Hex(csr!("mip"))
    );
    fstart_log::info!("entry a1 {}", Hex(csr!("mscratch")));
    // Vendor CSR 0x7c0: bit 0 I-cache, bit 1 D-cache enable.
    fstart_log::info!("cache control {}", Hex(csr!("0x7c0")));
    fstart_log::info!("k3: halting");
    fstart_arch::halt()
}
