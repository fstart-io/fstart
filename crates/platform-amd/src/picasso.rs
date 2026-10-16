// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native Picasso bring-up boundary. FSP calls are NOT silently skipped
//! in a production boot flow; this checkpoint only establishes the debug UART.
use fstart_driver_amd::{
    fch::{self, Fch},
    gpio::{self, Mmio, Pad, Pull},
    picasso::{EARLY_AOAC, UART_AOAC},
};

#[cfg(all(target_arch = "x86_64", target_os = "none"))]
core::arch::global_asm!(
    include_str!("../asm/picasso.S"),
    ap_reset_segment = const {
        let address = fstart_arch::x86::mp::AP_BOOTSTRAP_ADDRESS;
        assert!(address & 15 == 0 && address <= 0xffff0);
        address >> 4
    },
);

pub const LINKER_SCRIPT: &str = include_str!("../linker/picasso.ld");

#[derive(Debug)]
pub struct Config {
    uart: u8,
}
impl Config {
    pub const fn new() -> Self {
        Self { uart: 0 }
    }
    pub const fn uart(mut self, uart: u8) -> Self {
        self.uart = uart;
        self
    }
    pub const fn build(self) -> Self {
        assert!(self.uart < 4);
        self
    }
    pub const fn console_uart(&self) -> u8 {
        self.uart
    }
}
impl Default for Config {
    fn default() -> Self {
        Self::new().build()
    }
}

/// Unconditional, host-clean board identity and hardware facts.
pub trait BoardFacts {
    const CONFIG: &'static Config;
}

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    Gpio(gpio::Error),
    Fch(fch::Error),
}

/// A PSP-trained DRAM checkpoint, before any PCI/OS handoff. The caller provides
/// a 100-us delay independent of clock frequency. All MMIO is accessed with
/// tock-registers. No memory training is performed by x86.
/// # Safety
/// BSP only, interrupts disabled, cache disabled or correct AMD MTRRs already
/// installed, identity UC mappings for FCH. PSP has loaded the linker-defined
/// DRAM image and initialized APOB. This is NOT an S3 resume entry.
#[cfg(target_arch = "x86_64")]
pub unsafe fn console_checkpoint<B: BoardFacts>(
    mut delay_100us: impl FnMut(),
) -> Result<(), Error> {
    unsafe { fch::enable_acpimmio() };
    let mut fch = unsafe { Fch::new() };
    fch.enable_legacy_decode();
    let mut gpio = unsafe { Mmio::new() };
    // Power all common early devices before waiting, then the selected UART.
    fch.power_devices(EARLY_AOAC, 10000, &mut delay_100us)
        .map_err(Error::Fch)?;
    let uart = B::CONFIG.console_uart();
    fch.power_devices(&[UART_AOAC[usize::from(uart)]], 10000, &mut delay_100us)
        .map_err(Error::Fch)?;
    // Range_0 comes out of reset enabled; clear legacy UART aliases.
    fch.clear_legacy_uart_decode();
    let pins = match uart {
        0 => [
            Pad::native(138, 0, Pull::None),
            Pad::native(136, 0, Pull::None),
        ],
        1 => [
            Pad::native(143, 0, Pull::None),
            Pad::native(141, 0, Pull::None),
        ],
        2 => [
            Pad::native(137, 1, Pull::None),
            Pad::native(135, 1, Pull::None),
        ],
        3 => [
            Pad::native(140, 1, Pull::None),
            Pad::native(142, 1, Pull::None),
        ],
        _ => unreachable!(),
    };
    gpio::configure(&mut gpio, &pins, &[]).map_err(Error::Gpio)
}

/// Shared native checkpoint binary adapter, not a board-owned init sequence.
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
pub fn run_checkpoint<B: BoardFacts>() -> ! {
    use fstart_core::services::Console;
    use fstart_driver_amd::picasso::{UART_BASES, tsc};
    use fstart_driver_uart::ns16550::{AccessMode, Ns16550, Ns16550Config};
    let halt = || -> ! {
        loop {
            unsafe {
                core::arch::asm!("cli", "hlt", options(nomem, nostack));
            }
        }
    };
    // SAFETY: only entered by the BSP cold-reset assembly, cache disabled.
    let Some(mhz) = (unsafe { tsc::frequency_mhz() }) else {
        halt()
    };
    if unsafe { console_checkpoint::<B>(|| tsc::delay_us(mhz, 100)) }.is_err() {
        halt()
    }
    let mut console = Ns16550::new(Ns16550Config {
        regs: AccessMode::Mmio {
            base: UART_BASES[usize::from(B::CONFIG.console_uart())] as u64,
            reg_shift: 2,
            reg_width: 4,
        },
        clock_freq: 48_000_000,
        baud_rate: 115200,
    })
    .unwrap_or_else(|_| halt());
    if console.init().is_err() {
        halt()
    }
    for &byte in b"fstart Picasso: native PSP-trained DRAM console checkpoint\r\nNOT an OS boot: native silicon initialization remains incomplete.\r\n" {
        let _ = console.write_byte(byte);
    }
    let _ = console.flush();
    halt()
}

#[macro_export]
macro_rules! checkpoint_bin {
    ($board:ty) => {
        #[unsafe(no_mangle)]
        pub extern "C" fn fstart_picasso_main() -> ! {
            $crate::picasso::run_checkpoint::<$board>()
        }
        #[panic_handler]
        fn panic(_: &core::panic::PanicInfo) -> ! {
            loop {
                unsafe {
                    core::arch::asm!("cli", "hlt", options(nomem, nostack));
                }
            }
        }
    };
}
