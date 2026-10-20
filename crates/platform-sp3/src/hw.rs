//! Hardware handles the flow passes to the silicon drivers: config space,
//! the TSC timer, POST codes, the ECAM MSR and the FCH views.

use fstart_arch::x86::cpu::amd::msr::{MMIO_CFG_BASE, MmioCfgBase};
use fstart_arch::x86::cpu::msr::{MsrAccess, NativeMsr};
use fstart_core::services::Timer;
use fstart_driver_zen::fch::{Fch, Mmio, Ports, SmnWindow, acpimmio};
use fstart_driver_zen::smn::Smn;
use fstart_driver_zen::soc::Generation;
use tock_registers::interfaces::Writeable;

/// Config space of every function through the global ECAM base.
pub type Pci = fstart_pci::ecam::GlobalEcam;

/// The primary FCH: host ACPI-MMIO at `0xFED8_0000`, host MMIO and ports.
pub type PrimaryFch = Fch<Mmio, Mmio, Ports, Pci, Tsc>;
/// A secondary FCH (Naples die 1..7, Rome/Milan socket 1) through its SMN.
pub type SecondaryFch = Fch<SmnWindow<Pci>, Mmio, Ports, Pci, Tsc>;

/// The CPU's own MSRs.
pub fn msr() -> NativeMsr {
    // SAFETY: ring 0 on a Zen CPU; the flow only touches MSRs the CCX/SMM
    // reports name for this family.
    unsafe { NativeMsr::new() }
}

/// `PStateDef0`: the TSC runs at the P0 frequency on Zen (PPR "TSC").
const PSTATE_DEF0: u32 = 0xc001_0064;

/// Microsecond timer on the TSC, calibrated from the P0 definition. The
/// arch crate's generic calibration has no AMD path and assumes 1 GHz.
#[derive(Debug, Clone, Copy)]
pub struct Tsc {
    mhz: u64,
}

impl Tsc {
    pub fn new() -> Self {
        let p0 = fstart_driver_zen::ccx::power::decode_pstate(0, msr().read(PSTATE_DEF0));
        Self {
            mhz: p0.map_or(1000, |p| u64::from(p.mhz)).max(1),
        }
    }
}

impl Default for Tsc {
    fn default() -> Self {
        Self::new()
    }
}

impl Timer for Tsc {
    fn delay_us(&self, us: u64) {
        let ticks = us.saturating_mul(self.mhz);
        let start = fstart_arch::x86::rdtsc();
        while fstart_arch::x86::rdtsc().wrapping_sub(start) < ticks {
            core::hint::spin_loop();
        }
    }

    fn timestamp_us(&self) -> u64 {
        fstart_arch::x86::rdtsc() / self.mhz
    }
}

/// Write a POST code to port 80h (the BMC shows it).
pub fn post(code: u8) {
    // SAFETY: port 80h is the POST port on every SP3 board.
    unsafe { fstart_core::pio::outb(0x80, code) }
}

/// Program `MMIO_CFG_BASE` (C001_0058) for the generation's ECAM (SOC_DF
/// §3.2) and publish it to the PCI layer. The PSP/ABL leave the window
/// unset; every thread must get the same value (the MP code mirrors it).
pub fn enable_ecam(generation: Generation) -> (u64, u64) {
    let (base, buses) = generation.ecam();
    msr().reg(MMIO_CFG_BASE).write(
        MmioCfgBase::ENABLE::SET
            + MmioCfgBase::BUS_RANGE.val(u64::from(buses.trailing_zeros()))
            + MmioCfgBase::ADDRESS.val(base >> 20),
    );
    fstart_pci::ecam::init(base as usize);
    (base, u64::from(buses) << 20)
}

/// Socket 0's FCH as the host sees it.
pub fn primary_fch(timer: Tsc) -> PrimaryFch {
    // SAFETY: the boot flow owns the FCH's ACPI-MMIO block, MMIO, ports and
    // config space until the payload starts.
    unsafe {
        Fch {
            acpi: Mmio::new(acpimmio::BASE as usize),
            mmio: Mmio::new(0),
            io: Ports::new(),
            pci: Pci::default(),
            bus: 0,
            timer,
        }
    }
}

/// The FCH of the die/socket whose root bus is `bus`, through its SMN.
pub fn secondary_fch(bus: u8, timer: Tsc) -> SecondaryFch {
    let pci = Pci::default();
    // SAFETY: as `primary_fch`; the secondary's ports are only reached
    // through the DF legacy route the FCH tasks open themselves.
    unsafe {
        Fch {
            acpi: SmnWindow::new(Smn::new(pci, bus), acpimmio::SMN_BASE),
            mmio: Mmio::new(0),
            io: Ports::new(),
            pci,
            bus,
            timer,
        }
    }
}
