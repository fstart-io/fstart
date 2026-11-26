// SPDX-License-Identifier: MIT OR Apache-2.0
//! The SoCs of the RS I/O die: Genoa ([`crate::genoa`], SP5) and Siena
//! ([`crate::siena`], SP6), one socket each. They share their silicon
//! steps, which take the SoC as a type ([`Variant`]): DF4 ([`df::v4`]) with
//! the planner of the earlier fabrics, the RS NBIO of every IOS
//! ([`nbio::rs`]), MPIO ([`dxio::rs`]), SMU v13, the Songshan FCH's USB
//! ([`fch::songshan`]) and the RS cores ([`ccx::rs`]); the flow's services
//! are the shared ones.
//!
//! What a Genoa and a Siena differ in is data: the IOS a part has (Siena
//! fuses some off), the DRAM the PSP leaves the APOB and the reset image
//! in, the SMU's message range, the PM base, the SMBIOS socket name.
//!
//! [`df::v4`]: fstart_driver_amd::df::v4
//! [`nbio::rs`]: fstart_driver_amd::nbio::rs
//! [`dxio::rs`]: fstart_driver_amd::dxio::rs
//! [`fch::songshan`]: fstart_driver_amd::fch::songshan
//! [`ccx::rs`]: fstart_driver_amd::ccx::rs
pub mod acpi;
#[cfg(feature = "host")]
pub mod host;
#[cfg(all(feature = "bundle", target_os = "none"))]
mod init;
pub mod links;
#[cfg(all(feature = "bundle", target_os = "none"))]
mod mpio;
pub mod numa;
pub mod smm;
#[cfg(all(feature = "bundle", target_os = "none"))]
mod tables;
#[cfg(all(feature = "bundle", target_os = "none"))]
pub use init::Hw;

use core::marker::PhantomData;

use crate::layout::{Layout, Region};
use fstart_driver_amd::{
    ccx,
    df::v4::Ios,
    dxio::rs::mailbox::Encoding,
    fch::gpio::Pad,
    fch::songshan::{Perst, usb},
    nbio,
};

/// ECAM: 256 buses at 0xE000_0000 on segment 0, as both vendors have it.
pub const ECAM_BASE: u64 = 0xe000_0000;
pub const ECAM_BUSES: u16 = 256;

/// TSEG, at the top of the DRAM below 4 GiB.
pub const TSEG_SIZE: u32 = 8 << 20;

/// The most threads of one RS socket (12 CCDs of 8 cores, SMT).
pub const MAX_THREADS: usize = 192;

/// What a SoC of the RS die differs in.
pub trait Variant: Send + Sync + 'static {
    /// The log prefix and the name of its firmware tables.
    const NAME: &'static str;
    /// CPUID(1).EAX of its models, stepping masked off.
    const SIGNATURES: &'static [u32];
    const LAYOUT: Layout;
    /// Where the PSP writes the APOB (BIOS directory entry 0x61).
    const APOB: Region;
    /// The native EFI settings store in the ROM window, outside the FFS.
    const NVRAM: Option<Region> = None;
    /// Warm-reset-preserved RAM buffer for CrabEFI deferred runtime variable
    /// writes, inside `LAYOUT.early` and disjoint from stack, heap, and APOB.
    const DEFERRED_BUFFER: Option<Region> = None;
    /// The exclusive bound of the SMU firmware's message IDs.
    const SMU_ID_LIMIT: u32;
    /// The ACPI PM base the vendor's FCH init programs.
    const PMBASE: u16;
    /// The SMBIOS socket designation.
    const SOCKET: &'static str;
    /// The IOS the part has, from its fuses.
    fn ios(smn: &impl fstart_driver_amd::smn::SmnAccess) -> Option<heapless::Vec<Ios, 4>>;
}

/// A SoC of the RS die.
pub struct Rs<V>(PhantomData<V>);

impl<V: Variant> ccx::microcode::Family for Rs<V> {
    const SIGNATURES: &'static [u32] = V::SIGNATURES;
    const MAX_PATCHES: usize = 2;
}

/// How MPIO trains the board's ports.
#[derive(Debug, Clone)]
pub struct Mpio {
    /// The firmware has the PSP vet the requests (Siena's vendor firmware
    /// does; openSIL's Onyx configuration skips it).
    pub psp_vetting: bool,
    /// How the board's MPIO firmware takes the request number.
    pub encoding: Encoding,
    /// Soldered devices the boot needs: the link of their port trains,
    /// and enumeration finds them behind it ([`links::Soldered`]).
    pub required: heapless::Vec<links::Soldered, 32>,
    /// The links the PSP trained before MPIO, from the APCB (first and
    /// last lane): MPIO is not asked for them, and their root port is the
    /// live one MPIO did not train on the wrapper their lanes are on
    /// ([`links::psp_trained`]).
    pub psp_trained: &'static [(u8, u8)],
    /// The board's PCIe slot resets, released between MPIO's PERST and
    /// training phases.
    pub perst: &'static [Perst],
}

/// A board's configuration of an RS SoC.
#[derive(Clone)]
pub struct Config {
    /// The OEM CPM GPIO table and the board's pads, before MPIO
    /// (AmdCpmGpioInitPeim and the OEM PEI callbacks).
    pub gpio: &'static [Pad],
    pub nbio: nbio::rs::Policy,
    pub ccx: ccx::rs::Policy,
    pub mpio: Mpio,
    /// The xHCIs' wiring and tuning.
    pub usb: usb::Policy,
    /// Spread the PCIe reference clocks (the vendors' "Clock Spread
    /// Spectrum" setup option; on in openSIL). Some endpoints log
    /// correctable errors on a spread clock, as the MJ11's I210s did.
    pub spread_spectrum: bool,
    /// The first lane of the port of the board's onboard display, an MPIO
    /// port or one the PSP trained: the legacy VGA display is behind it
    /// ([`crate::soc::Init::display_port`]). `None`: the first
    /// VGA-compatible display.
    pub display: Option<u8>,
    /// An LPC/eSPI Super I/O or 8042 the OS should know of (FADT
    /// IAPC_BOOT_ARCH).
    pub legacy_devices: bool,
    /// The UMC channels (bit per channel) with DIMM slots.
    pub memory_channels: u16,
    /// The largest DIMM a slot takes, MiB.
    pub max_dimm_mib: u32,
    /// Publish the PSP's ACPI EINJ ([`smm`]): an OS can inject correctable
    /// and uncorrectable memory, PCIe and CXL errors anywhere, which the
    /// PSP alone validates. A test interface, off in the vendors' setup.
    pub einj: bool,
}
