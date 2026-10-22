// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native Picasso bring-up boundary. FSP calls are NOT silently skipped
//! in a production boot flow; this checkpoint only establishes the debug UART.
use fstart_driver_amd::{
    ccx::tsc::{Clock, DelayNs},
    fch::{
        self, Fch, espi,
        gpio::{self, Gpio, Pad, Pull},
        sandstone::preinit::SpiConfig,
    },
    soc::{
        Soc,
        picasso::{EARLY_AOAC, Picasso, UART_AOAC},
    },
};

#[cfg(all(target_arch = "x86_64", target_os = "none"))]
core::arch::global_asm!(
    include_str!("../asm/psp_reset.S"),
    ap_reset_segment = const {
        let address = fstart_arch::x86::mp::AP_BOOTSTRAP_ADDRESS;
        assert!(address & 15 == 0 && address <= 0xffff0);
        address >> 4
    },
);

#[cfg(all(target_arch = "x86_64", target_os = "none"))]
core::arch::global_asm!(
    ".code64",
    ".global _fstart_amd_long_mode_entry",
    "_fstart_amd_long_mode_entry:",
    "call fstart_picasso_main",
    "3: cli",
    "hlt",
    "jmp 3b",
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

/// Board wiring and the mainboard hook. The platform owns the order; boards
/// supply data.
pub trait BoardFacts {
    const CONFIG: &'static Config;
    /// SPI ROM read mode and speeds.
    const SPI: SpiConfig;
    /// eSPI link to the EC and its pads.
    const ESPI: Option<(espi::Config, &'static [Pad])> = None;
    /// GPIO tables programmed once the FCH is up, in order.
    const EARLY_GPIOS: &'static [&'static [Pad]] = &[];
    /// `DISABLE_SPI_FLASH_ROM_SHARING`: done before `mainboard_init`.
    const DISABLE_ROM_SHARING: bool = false;
    /// Mainboard init once the FCH, eSPI and early GPIOs are up: read board facts, program the remaining GPIOs.
    fn mainboard_init(_gpio: &Gpio<impl fch::Bus>, _clock: &mut impl Clock) {}
}

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    Gpio(gpio::Error),
    Fch(fch::Error),
}

/// A PSP-trained DRAM checkpoint, before any PCI/OS handoff. All MMIO is
/// accessed with tock-registers. No memory training is performed by x86.
/// # Safety
/// BSP only, interrupts disabled, cache disabled or correct AMD MTRRs already
/// installed, identity UC mappings for FCH. PSP has loaded the linker-defined
/// DRAM image and initialized APOB. This is NOT an S3 resume entry.
#[cfg(target_arch = "x86_64")]
pub unsafe fn console_checkpoint<B: BoardFacts>(
    delay: &mut impl DelayNs,
) -> Result<(), Error> {
    unsafe { fch::enable_acpimmio() };
    // SAFETY: exclusive FCH access; no config space before ECAM.
    let fch = Fch::new(unsafe { fch::NativeBus::new() }, fch::Acpimmio::DIE0);
    fch.enable_legacy_decode();
    // Power all common early devices before waiting, then the selected UART.
    fch.power_devices(EARLY_AOAC, 10000, delay)
        .map_err(Error::Fch)?;
    let uart = B::CONFIG.console_uart();
    fch.power_devices(&[UART_AOAC[usize::from(uart)]], 10000, delay)
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
    Gpio::new(fch, Picasso::GPIO_BANK)
        .configure(&pins, &[])
        .map_err(Error::Gpio)
}

/// TSEG size.
pub const TSEG_SIZE: u64 = 8 << 20;

/// Cold-boot entry after the PSP released the BSP: console, then early
/// silicon init from the APOB, running cached from here on.
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
pub fn run<B: BoardFacts>() -> ! {
    use fstart_driver_amd::{ccx::tsc, soc::picasso::UART_BASES};
    use fstart_driver_uart::ns16550::{AccessMode, Ns16550, Ns16550Config};
    // SAFETY: only entered by the BSP cold-reset assembly, cache disabled.
    let Some(mhz) = (unsafe { tsc::frequency_mhz() }).filter(|&mhz| mhz != 0) else {
        halt()
    };
    let mut timer = tsc::Tsc::new(mhz);
    if unsafe { console_checkpoint::<B>(&mut timer) }.is_err() {
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
    let result = unsafe { cold_boot(&mut timer) }.and_then(|state| {
        report(&state);
        unsafe { fch_init::<B>(&mut timer) }
    });
    match result {
        Ok(()) => fstart_log::info!("picasso: early init checkpoint reached, halting"),
        Err(step) => fstart_log::error!("picasso: {} failed", step),
    }
    halt()
}

/// FCH bring-up and the board's early wiring:
/// LPC/SPI aperture, eSPI, SPI ROM, SMBus, board GPIOs, ROM sharing, then
/// the mainboard hook.
/// # Safety
/// BSP only after `cold_boot`, exclusive FCH access.
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
unsafe fn fch_init<B: BoardFacts>(timer: &mut impl Clock) -> Result<(), &'static str> {
    use fstart_driver_amd::fch::sandstone::{legacy, preinit};
    // SAFETY: exclusive FCH access, ECAM published by `cold_boot`.
    let fch = Fch::new(unsafe { fch::NativeBus::new() }, fch::Acpimmio::DIE0);
    preinit::prepare_lpc(&fch).map_err(|_| "LPC setup")?;
    let gpio = Gpio::new(Fch::new(fch.bus(), fch.acpimmio()), Picasso::GPIO_BANK);
    if let Some((config, pads)) = &B::ESPI {
        gpio.configure(pads, &[]).map_err(|_| "eSPI GPIOs")?;
        let mut controller = unsafe { espi::Native::new(preinit::SPI_BASE as usize) };
        espi::initialize(&mut controller, timer, config).map_err(|_| "eSPI link")?;
        fstart_log::info!("picasso: eSPI link up");
    }
    preinit::configure_spi(&fch, B::SPI).map_err(|_| "SPI setup")?;
    preinit::initialize_smbus(&fch).map_err(|_| "SMBus setup")?;
    for table in B::EARLY_GPIOS {
        gpio.configure(table, &[]).map_err(|_| "board GPIOs")?;
    }
    if B::DISABLE_ROM_SHARING {
        legacy::disable_rom_sharing(fch.bus());
    }
    B::mainboard_init(&gpio, timer);
    Ok(())
}

/// # Safety
/// BSP only, cold boot, caches disabled, exclusive MSR/PCI access.
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
unsafe fn cold_boot(
    delay: &mut impl DelayNs,
) -> Result<crate::early::State, &'static str> {
    use crate::{early, layout};
    use fstart_arch::x86::cpu::msr::NativeMsr;
    use fstart_driver_amd::{
        apob::Apob,
        ccx::picasso as cpu,
        df,
        soc::picasso::{ECAM, Picasso},
    };
    let msr = unsafe { NativeMsr::new() };
    // MMCONF first, everything else uses it.
    cpu::enable_ecam(&msr);
    fstart_pci::ecam::init(ECAM as usize);
    let pci = fstart_pci::ecam::GlobalEcam;
    // SAFETY: the BSP owns the die's SMN and DF index pairs.
    let Some(die) = (unsafe { df::dies::<Picasso, _>(pci) }).next() else {
        return Err("no die");
    };
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
    let physical_address_bits = fstart_arch::x86::physical_address_bits() as u8;
    early::initialize(
        early::Inputs {
            apob: &apob,
            cpuid_signature: signature,
            cpuid_extended_ebx: extended_ebx,
            physical_address_bits,
            tseg_size: TSEG_SIZE,
        },
        &msr,
        &die,
        delay,
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
