// SPDX-License-Identifier: MIT OR Apache-2.0
//! The SoCs of the SP7 socket (WH I/O dies + Qilian FCH): currently Venice
//! ([`crate::venice`]), 1P and 2P.
//!
//! **Status.** Compiled for the GIGABYTE MD25-CG0 (1P) and host-tested;
//! never run on SP7 silicon. The sequences follow the vendors' code as
//! decompiled from the MD25-CG0 F01 and Dell PR9XJ (2P) ROMs (the research
//! reports name the functions); that reading is the only check against the
//! donors, nothing here was run against them. 2P has no board: it is
//! exercised by host tests only (`tests_2p`: 12 root bridges and 1024
//! threads through the PCI roots, the allocator and the ACPI tables).
//!
//! What the flow does, step by step (`init`):
//! - APOB V2 from `9/9` (memory map), `9/0x13` (NPS), `0xC/0x22` (sockets
//!   and IODs), `3/3` (core maps), `8/8` and `1/0x11` (DIMMs), `7/6` (ABL
//!   events), its page-rounded size reserved.
//! - DFv4.5 on every IOD of every socket ([`fabric`]): the ABL's bus maps
//!   resolved to root bridges by the IOS fabric IDs (one PCI segment: a map
//!   on another is refused), the same 32 MMIO and 16 I/O pairs written to
//!   every node, VGA routed by die 0 of each socket, the PIE's hardware
//!   asserts cleared, CF8 extended config off at ready-to-boot.
//! - WH NBIO on every root bridge (`nbio`, the vendor tables of
//!   `nbio::wh`): IOMMU (512 KiB) and IOAPIC BARs from the bottom of each
//!   root bridge's non-PCI pool (one allocator, `crate::nonpci`, which CXL
//!   continues from the top), IOAPIC IDs by system root bridge, the PSP BAR
//!   on socket 0; after training the requester IDs, AER straps and the
//!   performance tables through MPIO `0x26` (written, not read back), after
//!   enumeration RAS and the NBIF locks, at ready-to-boot the PCIe locks,
//!   MPIO clock gating and the SMU AER message. No IOHC TOM (the ABL's).
//! - Qilian FCH ([`fch`]): takeover, HW-ACPI, A-Link, sync flood; the
//!   secondary FCH of every other IOD relocated and initialized
//!   (HwAcpi, A-Link, SD off); a secondary that fails is logged and left
//!   out, the boot goes on. USB only with `Config::usb` (off on MD25).
//! - WH MPIO v2 per IOD (`dxio::wh`, `mpio`): ASKs, port binding by lane
//!   and kind, devfn remaps, straps, reconfigure, PERST release on every
//!   FCH, training, `UPDATE_LINK` and UBM PERST only with `ubm_hotplug`,
//!   hotplug only with `Mpio::hotplug`; bridges of untrained ports hidden.
//!   An ABL-trained BMC link (`Mpio::early_bmc`) is kept as trained; MD25
//!   sets none. Board devices: `Mpio::required` halts without them,
//!   `Mpio::expected` warns.
//! - Zen 6 / Zen 6c (`ccx::wh`): one AP at a time through its IOD's SMN,
//!   each checking in with the APIC ID its launch slot predicts; x2APIC
//!   unless no IOMMU is up; microcode from the FFS unless the ASP loads it.
//! - SMM in a 16 MiB TSEG (`smm`): the PSP's SmmInfo and P2C, machine-check
//!   thresholds and storms, SEL to the BMC, S5; EINJ only with
//!   `Config::einj` (off on MD25).
//! - CXL ([`cxl`], CEDT, `ACPI0016`/`ACPI0017`) only with `Config::cxl`
//!   (off on MD25).
//! - ACPI: FADT (direct ACPI mode, no SMI command port), MADT, MCFG, HPET,
//!   DSDT (per root bridge `_PRT` from the live IOHC devfns, one shared
//!   `_CPC`), IVRS (also handed to the PSP, C2P `0x3F`), SRAT/SLIT/MSCT,
//!   HEST, BERT, WSMT, EINJ and CEDT when on; SMBIOS with the DDR5 DIMMs.
//! - Flash ([`flash`]): the reset image both ISH slots name, read through
//!   ROM3 when mapped, else through SMN (MD25's ROM3 is above the 4 TiB
//!   identity map), only once a TPM answered; the FFS from ROM2, else from
//!   an SMN copy in DRAM below the firmware.
//!
//! Unverified, among others: the IOS fabric IDs when the DF names none, the
//! WH `ReservedRasEinj` hole type, the secondary FCH sequence and the
//! eSPI/KCS decodes on silicon, lane 129 being the MD25 BMC display, the
//! SMU v14 USB messages, 8-bit IOAPIC IDs, MPIO hotplug on socket 1.

pub mod acpi;
pub mod cxl;
pub mod fabric;
#[cfg(all(feature = "bundle", target_os = "none"))]
mod fch;
pub mod flash;
#[cfg(feature = "host")]
pub mod host;
#[cfg(all(feature = "bundle", target_os = "none"))]
mod init;
pub mod links;
#[cfg(all(feature = "bundle", target_os = "none"))]
mod mpio;
#[cfg(all(feature = "bundle", target_os = "none"))]
mod nbio;
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
    df::wh as df_wh,
    fch::{
        gpio::Pad,
        qilian::{self, Perst},
    },
};
use heapless::Vec;

/// ECAM: 256 buses at `0xE000_0000` on segment 0.
pub const ECAM_BASE: u64 = 0xe000_0000;
pub const ECAM_BUSES: u16 = 256;

/// How the APOB's memory holes are read on WH: `apob::sysmap`'s WH
/// classification (the vendor's `AmdMemoryHobInfoPeim`: UMA, MMIO, the
/// 1 TiB remap, every other type reserved). One name for what the memory
/// map, EINJ and SMBIOS read.
#[cfg_attr(not(all(feature = "bundle", target_os = "none")), allow(dead_code))]
pub(crate) const HOLES: fstart_driver_amd::psp::Variant = fstart_driver_amd::psp::Variant::Wh;

/// FCH IOAPIC ID (matches MADT and Qilian FCH defaults).
pub const FCH_IOAPIC_ID: u8 = 0;

/// TSEG at the top of DRAM below 4 GiB: 16 MiB fits the 1024 threads of 2P
/// at 8 KiB per SMM stack (8 MiB), their 0x400-byte save states (31 per
/// 64 KiB SMBASE window, about 2.1 MiB) and the handler image.
pub const TSEG_SIZE: u32 = 16 << 20;

/// ID of the NBIO IOAPIC of the system's first root bridge; root bridge `i`
/// (system order) has `IOAPIC_ID_BASE + i` (the vendor's
/// `PcdIoApicIdBase + index`, whose default is unknown). Right after the
/// FCH's, so 16 root bridges of 2P stay below `0xFF`.
pub const IOAPIC_ID_BASE: u8 = FCH_IOAPIC_ID + 1;

pub use fabric::{
    Bars, IOAPIC_ALIGN, IOAPIC_SIZE, IOMMU_BAR_SIZE, MAX_ROOT_BRIDGES, PSP_BAR_SIZE, place_bars,
};

/// The board's MPIO port description ([`PortDesc`], the vendor's `$A26`
/// descriptor as typed fields) and MPIO policy types.
pub use fstart_driver_amd::dxio::wh::{
    GlobalConfig as MpioGlobal, PortDesc,
    hotplug::{Policy as HotplugService, Slot as HotplugSlot},
    port::{
        AncOverride, Apic, Aspm, Compliance, Engine, Eq, Gen3Eq, GenEq, Hotplug, I2cSwitch, Npem,
        Pspp, SlotPower, Speed, Sris,
    },
    straps::{CoreFlags, Policy as PortPolicy},
};

/// One socket's ports, the vendor's `$A26` (socket 0) / `$A27` (socket 1)
/// table. Lanes are the socket's, `0..=135`.
#[derive(Debug, Clone, Copy)]
pub struct SocketTopology<'a> {
    pub socket: u8,
    pub ports: &'a [PortDesc<'a>],
}

/// The link the ABL trained before x86 (`EarlyBmcLinkTraining`,
/// `EarlyBmcLinkSocket`, `EarlyBmcLinkLaneNum`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EarlyBmc {
    pub socket: u8,
    /// A socket lane of the link.
    pub lane: u8,
}

/// The board's hotplug slots and MPIO's hotplug service policy.
#[derive(Debug, Clone, Copy)]
pub struct MpioHotplug {
    /// The expander wiring of each hotplug slot.
    pub slots: &'static [HotplugSlot],
    /// Requests `0x16`/`0x19`/`0x14`.
    pub service: HotplugService,
    /// Root-port setup (`PcieHotplugInitWh`).
    pub root_ports: fstart_driver_amd::dxio::wh::hotplug::InitPolicy,
}

/// What a SoC of the SP7 socket differs in.
pub trait Variant: Send + Sync + 'static {
    /// The log prefix and the name of its firmware tables.
    const NAME: &'static str;
    /// CPUID(1).EAX of its models, stepping masked off.
    const SIGNATURES: &'static [u32];
    const LAYOUT: Layout;
    /// Base physical address where the PSP writes the APOB V2 (BIOS `0x61`).
    const APOB_BASE: u64;
    /// Upper bound on APOB V2 size accepted from DRAM.
    const APOB_MAX_SIZE: u64;
    /// Physical SPI flash size reported to SMBIOS Type 0.
    const ROM_SIZE: u32;
    /// The native EFI settings store in the ROM window, outside the FFS.
    const NVRAM: Option<Region> = None;
    /// The ACPI PM base the FCH uses.
    const PMBASE: u16;
}

/// A SoC of the SP7 socket.
pub struct Sp7<V>(PhantomData<V>);

impl<V: Variant> ccx::microcode::Family for Sp7<V> {
    const SIGNATURES: &'static [u32] = V::SIGNATURES;
    const MAX_PATCHES: usize = 4;
}

/// How the board configures MPIO and slot resets on SP7.
#[derive(Debug, Clone)]
pub struct Mpio {
    /// Request `0x02`'s policy.
    pub global: MpioGlobal,
    /// Send the ports' ancillary records (`MPIOAncDataSupport`).
    pub ancillary: bool,
    /// Wait after PERST release, ms (`AfterResetDelay`).
    pub after_reset_delay_ms: u32,
    /// UBM hotplug (`CfgHotplugUMBSupport`): `UPDATE_LINK` (`0x322`) after
    /// training and PERST release of UBM ports (`0x2A`).
    pub ubm_hotplug: bool,
    /// The per-port strap and private-register policy.
    pub ports: PortPolicy,
    /// Per-core straps.
    pub cores: CoreFlags,
    /// Hotplug slots (`None`: no hotplug service).
    pub hotplug: Option<MpioHotplug>,
    /// The ABL-trained link, if APCB enables early BMC training.
    pub early_bmc: Option<EarlyBmc>,
    /// Soldered board devices the boot needs: their link trains and
    /// enumeration finds them behind their port, else the boot halts.
    pub required: Vec<links::Soldered, 32>,
    /// Soldered board devices that should be there, but whose absence is
    /// only reported: a link whose training is not proven (an ABL-trained
    /// BMC link fstart does not know of, a lane mapping not seen on
    /// silicon).
    pub expected: Vec<links::Soldered, 32>,
    /// PCIe slot resets released during link bring-up.
    pub perst: &'static [Perst],
}

/// A board's configuration of an SP7 SoC.
#[derive(Clone)]
pub struct Config {
    /// Early board GPIO pad table programmed before MPIO.
    pub gpio: &'static [Pad],
    /// Zen 6 per-thread policy (vendor defaults), the APIC mode among it:
    /// x2APIC unless no IOMMU is up.
    pub ccx: ccx::wh::Policy,
    /// WH NBIO policy (Dell build defaults).
    pub nbio: fstart_driver_amd::nbio::wh::Policy,
    /// MPIO / PERST configuration.
    pub mpio: Mpio,
    /// Optional Qilian xHCI USB cold-boot configuration (`None` leaves USB
    /// untouched until SMU v14 USB messages are verified on SP7 silicon).
    pub usb: Option<qilian::usb::Policy>,
    /// First lane of the board's onboard display root port on socket 0
    /// (`None`: first VGA).
    pub display: Option<u8>,
    /// An LPC/eSPI Super I/O or legacy BMC console is present (FADT
    /// `IAPC_LEGACY_DEVICES`).
    pub legacy_devices: bool,
    /// True if host-side IPMI KCS I/O decode was established during early init.
    /// Forwarded to SMM configuration and boot-time SEL reporting.
    pub kcs_decoded: bool,
    /// Populated UMC memory channels per socket (bit per channel; SP7 has 16
    /// per socket). Only these channels' DIMM slots are reported.
    pub memory_channels: [u16; df_wh::MAX_SOCKETS as usize],
    /// Largest DIMM a slot takes, MiB.
    pub max_dimm_mib: u32,
    /// Publish the PSP's ACPI EINJ service when available (opt-in until the WH
    /// `ReservedRasEinj` hole ID and command ABI are verified on silicon).
    pub einj: bool,
    /// The board's CXL ports and memory policy (`None`: no CXL).
    pub cxl: Option<cxl::Config>,
}

#[cfg(test)]
mod tests_2p;

#[cfg(test)]
mod tests {
    use super::*;

    /// The IOAPIC IDs of 2P B0's 12 root bridges (1..=12) and the FCH's
    /// (0) fit a 4-bit ID register (NBIO §1 item 12 warns of a wrap above
    /// 15). A part with 16 root bridges would reach 16: whether WH's
    /// registers hold 8 bits is not verified, and `sp7::tables` leaves out
    /// an IOAPIC whose ID does not read back.
    #[test]
    fn ioapic_ids_fit_2p() {
        let rbs = 2 * 2 * usize::from(fstart_driver_amd::df::wh::devices::B0.ios.count);
        assert_eq!(rbs, 12);
        let last = fstart_driver_amd::nbio::wh::ioapic_id(IOAPIC_ID_BASE, rbs - 1);
        assert_eq!(last, Some(12));
        assert!(last.unwrap() <= 0xf && FCH_IOAPIC_ID < IOAPIC_ID_BASE);
    }

    /// The TSEG holds 1024 CPUs (2P) with a generous handler image; the
    /// board build checks the real image (`build.rs`) for its own count.
    #[test]
    fn tseg_fits_1024_cpus() {
        use fstart_smm::{CpuSmmLayout, SMM_IDENTITY_TABLE_SIZE, SmramLayout};
        let layout = SmramLayout {
            smram_base: 0x7f00_0000,
            smram_size: TSEG_SIZE.into(),
            entry_count: 1024,
            save_state_size: 0x400,
            stack_size: crate::host::SMM_STACK_SIZE,
            entry_stub_size: 0x400,
            handler_mem_size: 4 << 20,
            page_table_size: SMM_IDENTITY_TABLE_SIZE,
        };
        let zero = CpuSmmLayout {
            smbase: 0,
            entry_addr: 0,
            save_state_base: 0,
            save_state_top: 0,
            stack_bottom: 0,
            stack_top: 0,
        };
        let mut out = std::vec![zero; 1024];
        assert!(fstart_smm::compute_cpu_layout(&layout, &mut out).is_ok());
    }
}
