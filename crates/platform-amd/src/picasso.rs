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

/// TSEG size.
pub const TSEG_SIZE: u64 = 8 << 20;

/// Cold-boot entry after the PSP released the BSP: console, then early
/// silicon init from the APOB, running cached from here on.
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
pub fn run<B: BoardFacts>() -> ! {
    use fstart_core::services::Console;
    use fstart_driver_amd::picasso::{UART_BASES, tsc};
    use fstart_driver_uart::ns16550::{AccessMode, Ns16550, Ns16550Config};
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
    // SAFETY: `console` lives until this never-returning function halts.
    unsafe { fstart_log::init(&console) };
    fstart_log::info!("fstart: AMD Picasso cold boot, TSC {} MHz", mhz);
    match unsafe { cold_boot(&mut |us| tsc::delay_us(mhz, us)) } {
        Ok(state) => report(&state),
        Err(step) => fstart_log::error!("picasso: {} failed", step),
    }
    fstart_log::info!("picasso: early init checkpoint reached, halting");
    halt()
}

/// # Safety
/// BSP only, cold boot, caches disabled, exclusive MSR/PCI access.
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
unsafe fn cold_boot(delay_us: &mut impl FnMut(u32)) -> Result<crate::early::State, &'static str> {
    use crate::{early, layout};
    use fstart_driver_amd::picasso::{
        ECAM,
        apob::Apob,
        cpu::{self, NativeMsr},
        registers::{FabricPort, SmnPort},
    };
    let mut msr = unsafe { NativeMsr::new() };
    // MMCONF first, everything else uses it.
    cpu::enable_ecam(&mut msr);
    fstart_pci::ecam::init(ECAM as usize);
    let pci = fstart_pci::ecam::GlobalEcam;
    let mut smn = unsafe { SmnPort::new(pci) };
    let mut df = unsafe { FabricPort::new(pci) };
    // SAFETY: the PSP wrote the APOB here before releasing the BSP; the
    // early reservation keeps it.
    let apob = unsafe {
        core::slice::from_raw_parts(layout::APOB_BASE as *const u8, layout::APOB_SIZE as usize)
    };
    let apob = Apob::parse(apob)
        .and_then(|apob| apob.memory_map())
        .map_err(|_| "APOB parse")?;
    fstart_log::info!(
        "picasso: APOB top of memory {}, {} hole(s)",
        fstart_log::Hex(apob.top),
        apob.holes().len()
    );
    let (signature, _, _, _) = fstart_arch::x86::cpuid(1);
    let (_, extended_ebx, _, _) = fstart_arch::x86::cpuid(0x8000_0001);
    let physical_address_bits = (fstart_arch::x86::cpuid(0x8000_0008).0 & 0xff) as u8;
    early::initialize(
        early::Inputs {
            apob: &apob,
            cpuid_signature: signature,
            cpuid_extended_ebx: extended_ebx,
            physical_address_bits,
            tseg_size: TSEG_SIZE,
        },
        &mut msr,
        &mut smn,
        &mut df,
        delay_us,
    )
    .map_err(|error| match error {
        early::Error::Cpu(_) => "CPU identification",
        early::Error::Cache(_) => "MTRR setup",
        early::Error::Memory(_) => "memory map",
        early::Error::Fabric(_) => "data fabric",
        early::Error::Nbio(_) => "NBIO early init",
    })
}

#[cfg(all(target_arch = "x86_64", target_os = "none"))]
fn report(state: &crate::early::State) {
    use fstart_log::Hex;
    fstart_log::info!(
        "picasso: silicon {}, TSEG {} + {}",
        Hex(u64::from(state.capabilities.silicon_id)),
        Hex(state.memory.tseg_base),
        Hex(state.memory.tseg_size)
    );
    for entry in state.memory.state().entries() {
        let (addr, size, kind) = (entry.addr, entry.size, entry.kind);
        fstart_log::info!(
            "picasso: e820 {}..{} type {}",
            Hex(addr),
            Hex(addr + size),
            kind
        );
    }
}

#[cfg(all(target_arch = "x86_64", target_os = "none"))]
fn halt() -> ! {
    loop {
        unsafe {
            core::arch::asm!("cli", "hlt", options(nomem, nostack));
        }
    }
}

#[macro_export]
macro_rules! checkpoint_bin {
    ($board:ty) => {
        #[unsafe(no_mangle)]
        pub extern "C" fn fstart_picasso_main() -> ! {
            $crate::picasso::run::<$board>()
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
