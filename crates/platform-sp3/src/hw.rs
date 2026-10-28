//! Hardware handles the flow passes to the silicon drivers: config space,
//! the TSC timer, POST codes, the ECAM MSR and the FCH views.

use core::num::NonZeroU32;
use fstart_arch::x86::cpu::amd::msr::{MMIO_CFG_BASE, MmioCfgBase};
use fstart_arch::x86::cpu::msr::{MsrAccess, NativeMsr};

use fstart_driver_amd::fch::{Bus, NativeBus};
use fstart_driver_amd::smn::Smn;
use fstart_driver_amd::sp3::fch::{Fch, Mmio, Ports, SmnWindow, acpimmio};
use fstart_driver_amd::sp3::soc::Generation;
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

/// Microsecond timer on the TSC.
pub type Tsc = fstart_driver_amd::ccx::tsc::Tsc;

/// SMU message timing on SP3: 10-us polls, each wait bounded by 1 s
/// unless a step needs longer.
#[allow(
    dead_code,
    reason = "the boot flow and the SMM handler, not the host build"
)]
pub(crate) const SMU_POLL_US: u32 = 10;
#[allow(
    dead_code,
    reason = "the boot flow and the SMM handler, not the host build"
)]
pub(crate) const SMU_TIMEOUT_US: u32 = 1_000_000;

/// The TSC, at 1 GHz if its P-state is disabled.
pub fn tsc() -> Tsc {
    const GHZ: NonZeroU32 = NonZeroU32::new(1000).unwrap();
    let mhz = fstart_arch::x86::cpu::amd::msr::tsc_mhz(&msr()).and_then(NonZeroU32::new);
    Tsc::new(mhz.unwrap_or(GHZ))
}

/// Write a POST code to port 80h (the BMC shows it).
pub fn post(code: u8) {
    // SAFETY: an I/O port write only; port 80h is the POST port on every
    // SP3 board.
    unsafe { NativeBus::<false>::without_config() }
        .reg(fstart_driver_amd::fch::POST_CODE)
        .set(code)
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
            acpi: SmnWindow::new(Smn::on_bus(pci, bus), acpimmio::SMN_BASE),
            mmio: Mmio::new(0),
            io: Ports::new(),
            pci,
            bus,
            timer,
        }
    }
}
