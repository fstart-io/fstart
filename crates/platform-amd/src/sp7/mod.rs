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
//! - ECAM: `MMIO_CFG_BASE` as the ABL left it, else the board's APCB
//!   window ([`Config::ecam`]); up to 8 PCI segments of 256 buses, segment
//!   `s` at `base + s * 256 MiB` (MCFG, `_SEG`, IVRS, the payload).
//! - DFv4.5 on every IOD of every socket ([`fabric`]): the ABL's bus maps,
//!   rewritten before NBIO for a board's request or a map the ECAM does not
//!   decode ([`Config::bus_request`], the vendor's PEI planner), resolved to
//!   root bridges by the IOS fabric IDs on any segment of the window, the
//!   same 32 MMIO and 16 I/O pairs written to every node, VGA routed by die
//!   0 of each socket, the PIE's hardware asserts cleared, CF8 extended
//!   config off at ready-to-boot.
//! - WH NBIO on every root bridge (`nbio`, the vendor tables of
//!   `nbio::wh`): IOMMU (512 KiB) and IOAPIC BARs from the bottom of each
//!   root bridge's non-PCI pool (one allocator, `crate::nonpci`, which CXL
//!   continues from the top), IOAPIC IDs by system root bridge, the PSP BAR
//!   on socket 0; after training the requester IDs, AER straps and the
//!   performance tables through MPIO `0x26` (written, not read back), after
//!   enumeration RAS and the NBIF locks, at ready-to-boot the PCIe locks,
//!   MPIO clock gating and the SMU AER message. No IOHC TOM (the ABL's).
//!   The SMU BAR for HSMP where nothing decoded it ([`Devices::hsmp`]).
//! - SMU v14 `SmuInitialization` on every socket after training (`smu`):
//!   feature controls, the PPTable with the board's power limits
//!   ([`Config::smu`]), the message sequence.
//! - Qilian FCH ([`fch`]): takeover, HW-ACPI with its register steps
//!   (watchdog decode, RTC extension, PrePcie, HPET, pre-OS), the FCH
//!   watchdog stopped and given the period its WDAT publishes
//!   ([`Devices::wdat`]), the ISA step only when the board opts in
//!   ([`Devices::isa`]: both donors drop every LPC decode), A-Link, sync
//!   flood; the board's I2C controllers ([`Devices::i2c`]); an FCH UART as
//!   a board's console ([`console`]); the
//!   secondary FCH of every other IOD relocated and initialized
//!   (HwAcpi, A-Link, SD off); a secondary that fails is logged and left
//!   out, the boot goes on. USB only with `Config::usb` (off on MD25).
//! - WH MPIO v2 per IOD (`dxio::wh`, `mpio`): ASKs, port binding by lane
//!   and kind, devfn remaps, straps, reconfigure, PERST release on every
//!   FCH, training, `UPDATE_LINK` and UBM PERST only with `ubm_hotplug`,
//!   hotplug only with `Mpio::hotplug`; bridges of untrained ports hidden.
//!   An ABL-trained BMC link (`Mpio::early_bmc`) is kept as trained (the
//!   MD25's AST2700 display at socket 0 lane `0x80`). Board devices:
//!   `Mpio::required` halts without them, `Mpio::expected` warns.
//! - Zen 6 / Zen 6c (`ccx::wh`): one AP at a time through its IOD's SMN,
//!   each checking in with the APIC ID its launch slot predicts; x2APIC
//!   unless no IOMMU is up; microcode from the FFS unless the ASP loads it.
//! - SMM in a 16 MiB TSEG (`smm`): the PSP's SmmInfo and P2C, machine-check
//!   thresholds and storms, firmware-first PCIe/CXL errors (the AER
//!   mechanism in effect, `ras`: the NBIO policy's, firmware-first by
//!   default, OS-first only where the handler cannot reach the ECAM, which
//!   it maps above 4 GiB too; it reaches a CXL register block only inside
//!   its root bridge's PCI windows), SEL to the BMC, `LockFCHReg` at
//!   ready-to-boot, S5; EINJ only with `Config::einj` (off on MD25).
//! - CXL ([`cxl`], CEDT, `ACPI0016`/`ACPI0017`) only with `Config::cxl`
//!   (on on MD25, every port, soft-reserved memory); the runtime APCB
//!   region update, which writes the booted ISH slot's APCB in the SPI
//!   flash and warm-resets ([`cxl_apcb`]), only with `apcb_update` (off on
//!   MD25).
//! - ACPI: FADT (direct ACPI mode, no SMI command port), MADT, MCFG, HPET,
//!   DSDT (per root bridge `_PRT` from the live IOHC devfns, one shared
//!   `_CPC`), IVRS (also handed to the PSP, C2P `0x3F`), SRAT/SLIT/MSCT,
//!   HEST (with the firmware-first GHESs, `AERR` and `_L14`), BERT, WSMT,
//!   WDAT and HSMP when on, EINJ and CEDT when on, HMAT with more than one
//!   domain (optionally one per CCD, [`Config::ccx_as_numa`]); SMBIOS with
//!   the DDR5 DIMMs.
//! - Flash ([`flash`]): the reset image both ISH slots name, read through
//!   ROM3 when mapped (the identity map covers 128 TiB), else through SMN,
//!   only once a TPM answered; the FFS from ROM2, else from
//!   an SMN copy in DRAM below the firmware.
//! - Resets (`reset`): the last reset read before the takeover and
//!   logged; every firmware warm reset (the PSP's ResetRequired on an A/B
//!   flash, a CXL APCB update) through Qilian's recipe and a guard in CMOS
//!   bank 1 that counts on its own record, not on PMxC0, and refuses past
//!   its per-reason limit or a fourth in a row, erased at BootDone; the
//!   payload's resets as the vendor's ResetSystem (a cold one is Qilian's
//!   full reset); the PSP's recovery firmware halts with POST 2Dh;
//!   GIGABYTE's APCB-recovery CMOS handshake.
//!
//! Unverified, among others: the IOS fabric IDs when the DF names none, the
//! PSP's EINJ command ABI, the secondary FCH sequence and the
//! eSPI/KCS decodes on silicon, the state of the MD25's ABL-trained
//! lane-128 display link, firmware-first AER and the runtime CXL APCB
//! write on silicon, the SMU v14 USB messages, 8-bit IOAPIC IDs. MPIO hotplug on 2P is the
//! vendor's (2P_COMPLETE §2.17), as read from its code.

pub mod acpi;
pub mod apcb;
pub mod console;
pub mod cxl;
pub mod cxl_apcb;
pub mod cxl_memory;
pub mod fabric;
#[cfg(all(feature = "bundle", target_os = "none"))]
mod fch;
pub mod flash;
#[cfg(feature = "host")]
pub mod host;
#[cfg(all(feature = "bundle", target_os = "none"))]
mod hsmp;
#[cfg(all(feature = "bundle", target_os = "none"))]
mod init;
pub mod links;
#[cfg(all(feature = "bundle", target_os = "none"))]
mod mpio;
#[cfg(all(feature = "bundle", target_os = "none"))]
mod nbio;
pub mod numa;
#[cfg(all(feature = "bundle", target_os = "none"))]
mod ras;
#[cfg(all(feature = "bundle", target_os = "none"))]
mod reset;
pub mod smm;
#[cfg(all(feature = "bundle", target_os = "none"))]
mod smu;
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

/// The ECAM window fstart programs when the ABL left `MMIO_CFG_BASE` off
/// and the board names none ([`Config::ecam`]): one segment at
/// `0xE000_0000`. The donors' APCBs put it elsewhere (Dell
/// `0x1000_0000_0000`, 2 segments; GIGABYTE `0x2FFC_8000_0000`, one;
/// 2P_COMPLETE §2.2), and the vendor requires the x86 ECAM to equal the
/// APCB's `DfPciMmio*` tokens, so a board names its APCB's.
pub const DEFAULT_ECAM: ccx::msr::EcamWindow = match ccx::msr::EcamWindow::new(0xe000_0000, 1) {
    Some(window) => window,
    None => panic!(),
};

/// Buses a board asks for, per socket and socket-wide physical root bridge
/// (`die * root bridges per IOD + rb`), `0` to keep the ABL's count (the
/// vendor's `PciBusNumber[socket][rb]`, 2P_COMPLETE §2.10).
pub type BusRequest = [[u16; 2 * fstart_driver_amd::nbio::wh::MAX_ROOT_BRIDGES_PER_IOD as usize];
    df_wh::MAX_SOCKETS as usize];

/// How the APOB's memory holes are read on WH: `apob::sysmap`'s WH
/// classification (the vendor's `AmdMemoryHobInfoPeim`: UMA, MMIO, the
/// 1 TiB remap, every other type reserved). One name for what the memory
/// map, EINJ and SMBIOS read.
#[cfg_attr(not(all(feature = "bundle", target_os = "none")), allow(dead_code))]
pub(crate) const HOLES: fstart_driver_amd::psp::Variant = fstart_driver_amd::psp::Variant::Wh;

/// FCH IOAPIC ID: both donors' `PcdCfgFchIoapicId` (`0x80`; Dell
/// AmdNbioDxe `0x125ce`, VALUES.md #9, 2P_COMPLETE §2.20).
pub const FCH_IOAPIC_ID: u8 = 0x80;

/// The SoC's sync-flood default: reset the CPUs, at once (openSIL's
/// `SyncFlood::DEFAULT`; Dell resets after 5, GIGABYTE does not reset, so a
/// board names its own, [`Config::sync_flood`]).
pub const SYNC_FLOOD: qilian::hwacpi::SyncFlood = qilian::hwacpi::SyncFlood::DEFAULT;

/// TSEG at the top of DRAM below 4 GiB: 16 MiB fits the 1024 threads of 2P
/// at 8 KiB per SMM stack (8 MiB), their 0x400-byte save states (31 per
/// 64 KiB SMBASE window, about 2.1 MiB) and the handler image.
pub const TSEG_SIZE: u32 = 16 << 20;

/// ID of the NBIO IOAPIC of the system's first root bridge; root bridge `i`
/// (system order) has `IOAPIC_ID_BASE + i`: both donors'
/// `PcdCfgGnbIoapicId` `0xF0` (Dell AmdNbioDxe `0x16da5..0x16fbb`,
/// VALUES.md #9; the Dell 2P MADT template's `0xF0 + i`). 2P B0's 12 root
/// bridges end at `0xFB`, 16 would reach `0xFF`.
pub const IOAPIC_ID_BASE: u8 = 0xf0;

pub use fabric::{
    Bars, IOAPIC_ALIGN, IOAPIC_SIZE, IOMMU_BAR_SIZE, MAX_ROOT_BRIDGES, PSP_BAR_SIZE, place_bars,
};

/// The board's MPIO port description ([`PortDesc`], the vendor's `$A26`
/// descriptor as typed fields) and MPIO policy types.
pub use fstart_driver_amd::dxio::wh::{
    GlobalConfig as MpioGlobal, PortDesc,
    bifurcation::CorePolicy,
    hotplug::{Policy as HotplugService, Slot as HotplugSlot},
    port::{
        AncOverride, Apic, Aspm, Compliance, Engine, Eq, Gen3Eq, GenEq, Hotplug, I2cSwitch, Npem,
        Pspp, SlotPower, Speed, Sris, UaLink, UaLinkPstate,
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
    /// The PCIe cores' wrapper flags (straps of every port and core).
    pub cores: CoreFlags,
    /// The MPIO input-block values of the core-level steps.
    pub core_policy: CorePolicy,
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
    /// WH NBIO policy (both donors' build values, Dell's where they
    /// differ; firmware-first AER, see `aer_mechanism`, OS-first in effect
    /// only where the SMM handler cannot reach the ECAM: `smm::Aer`).
    pub nbio: fstart_driver_amd::nbio::wh::Policy,
    /// What `SmuInitialization` sends every socket's SMU (the PPTable with
    /// the board's platform power limits, HSMP, the feature overrides, ...;
    /// `smu::v14::init::Policy::DEFAULT` is both donors' values with fused
    /// limits).
    pub smu: fstart_driver_amd::smu::v14::init::Policy,
    /// What a sync flood does (`PcdResetCpuOnSyncFlood`,
    /// `PcdDelayResetCpuOnSyncFlood`): the donors differ (Dell reset after
    /// 5, GIGABYTE no reset), so it is the board's; [`SYNC_FLOOD`] is the
    /// SoC's default.
    pub sync_flood: qilian::hwacpi::SyncFlood,
    /// MPIO / PERST configuration.
    pub mpio: Mpio,
    /// Optional Qilian xHCI USB cold-boot configuration per socket (its
    /// die-0 FCH; socket 1 has its own PHY tables and enables, SilVenicePei
    /// `0xf316a0`/`0xf31950`, 2P_COMPLETE §2.18). `None` leaves a socket's
    /// USB untouched until SMU v14 USB messages are verified on SP7
    /// silicon.
    pub usb: [Option<qilian::usb::Policy>; df_wh::MAX_SOCKETS as usize],
    /// The ECAM window of the board's APCB (`DfPciMmioBaseLo/Hi`,
    /// `DfPciMmioSize`, which the board sets in its `apcb-tokens`; a host
    /// test checks they agree, [`apcb::disagreement`]), programmed when
    /// the ABL left `MMIO_CFG_BASE` off (`ccx::msr::adopt_ecam`); `None`:
    /// [`DEFAULT_ECAM`].
    pub ecam: Option<ccx::msr::EcamWindow>,
    /// Buses per root bridge ([`BusRequest`]): the bus maps are rewritten
    /// before NBIO when they differ, or when the ABL's do not fit the ECAM
    /// (`sp7::fabric::bus_plan`). `None`: the ABL's counts.
    pub bus_request: Option<&'static BusRequest>,
    /// One proximity domain per CCD (`PcdAmdFabricCcxAsNumaDomain`, Dell
    /// attribute `CcxAsNumaDomain`, default off), each within its NPS
    /// domain ([`numa`]).
    pub ccx_as_numa: bool,
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
    /// Publish the PSP's ACPI EINJ service when available (opt-in until its
    /// command ABI is verified on silicon); the buffer is the one the PSP
    /// names (`MP0_C2PMSG_31`), in reserved memory.
    pub einj: bool,
    /// The board's CXL ports and memory policy (`None`: no CXL).
    pub cxl: Option<cxl::Config>,
    /// The FCH and NBIO devices and their inputs ([`Devices`]).
    pub devices: Devices,
}

/// The FCH and NBIO devices an SP7 board describes or turns on, and the
/// vendor inputs of their steps (DEVICES.md). [`Default`] is the Dell
/// PR9XJ build's where it is known, but for what a board opts into: the
/// ISA step, the WDAT, the I2C controllers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Devices {
    /// The FCH I2C controllers (0 and 2, `qilian::i2c`) to power and leave
    /// set up at the vendor's standard-mode timing; each is described
    /// (`AMDI0010`). The vendor's map turns on 0..5 (`FchRTDeviceEnableMap`
    /// 0x7E0), GIGABYTE's DSDT describes controller 2.
    pub i2c: &'static [u8],
    /// HSMP for the OS (`PcdCfgHSMPSupport`, Dell 1): the SMU BAR where the
    /// firmware before left none, an `AMDI0097` device per socket and the
    /// `HSMP` table ([`acpi::hsmp`](crate::acpi::hsmp)).
    pub hsmp: bool,
    /// The IOMMU's SR-IOV BARs (256 MiB and 4 MiB per IOMMU, Dell 1/1),
    /// from the root bridge's non-PCI pool; off: no vIOMMU, and a pool
    /// below 4 GiB has no 260 MiB to spare.
    pub iommu_vf: bool,
    /// The FCH HW-ACPI register steps' inputs (`qilian::pm`).
    pub fch_reset: qilian::pm::Reset,
    pub fch_misc: qilian::pm::Misc,
    /// `PcdNoClearS5ResetSts` (Dell 0): keep the S5 reset status at pre-OS.
    pub keep_s5_reset_status: bool,
    /// The LPC bridge and SPI controller step (`qilian::isa`), which a
    /// board opts into: both donors' builds drop every LPC decode (their
    /// BMC is on eSPI) and change the SPI normal-read clock
    /// ([`qilian::isa::Policy::DELL`], `GIGABYTE`). `None` (the default):
    /// the step does not run, and the LPC decodes and SPI clocks stay as
    /// the ABL left them, for a board with an LPC Super I/O, TPM or BMC.
    pub isa: Option<qilian::isa::Policy>,
    /// The FCH watchdog for the OS: counting in this period
    /// (`qilian::pm::watchdog_clock`, set at the HW-ACPI step) and
    /// published as a WDAT with it (`qilian::pm::WDAT`; GIGABYTE's 1000 ms
    /// is [`qilian::pm::WatchdogClock::S1`], Dell's 600 ms no counter
    /// period), needing its decode (`fch_reset.watchdog_decode`). `None`:
    /// no WDAT, the counter left as reset leaves it.
    pub wdat: Option<qilian::pm::WatchdogClock>,
}

impl Default for Devices {
    fn default() -> Self {
        Self {
            i2c: &[],
            hsmp: true,
            iommu_vf: false,
            fch_reset: qilian::pm::Reset::DEFAULT,
            fch_misc: qilian::pm::Misc::DEFAULT,
            keep_s5_reset_status: false,
            isa: None,
            wdat: None,
        }
    }
}

#[cfg(test)]
mod tests_2p;
#[cfg(test)]
mod values_tests;

#[cfg(test)]
mod tests {
    use super::*;

    /// The IOAPIC IDs of 2P B0's 12 root bridges (`0xF0..=0xFB`) and the
    /// FCH's (`0x80`) are the donors' (8-bit IDs, as the vendor writes
    /// them; `sp7::tables` leaves out an IOAPIC whose ID does not read
    /// back). The 16 root bridges `MAX_ROOT_BRIDGES` allows end at `0xFF`,
    /// unique and apart from the FCH's: MADT and IVRS IDs are 8 bits.
    #[test]
    fn ioapic_ids_fit_2p() {
        let rbs = 2 * 2 * usize::from(fstart_driver_amd::df::wh::devices::B0.ios.count);
        assert_eq!(rbs, 12);
        let last = fstart_driver_amd::nbio::wh::ioapic_id(IOAPIC_ID_BASE, rbs - 1);
        assert_eq!(last, Some(0xfb));
        assert_eq!(
            fstart_driver_amd::nbio::wh::ioapic_id(IOAPIC_ID_BASE, 15),
            Some(0xff)
        );
        assert_eq!(
            fstart_driver_amd::nbio::wh::ioapic_id(IOAPIC_ID_BASE, 16),
            None
        );
        const { assert!(FCH_IOAPIC_ID < IOAPIC_ID_BASE) };
        let ids: std::vec::Vec<u8> = (0..MAX_ROOT_BRIDGES)
            .filter_map(|i| fstart_driver_amd::nbio::wh::ioapic_id(IOAPIC_ID_BASE, i))
            .collect();
        assert_eq!(ids.len(), MAX_ROOT_BRIDGES);
        assert!(ids.iter().all(|&id| id != FCH_IOAPIC_ID));
        assert!(ids.windows(2).all(|w| w[0] < w[1]), "unique");
    }

    /// The TSEG holds 1024 CPUs (2P) with a generous handler image; the
    /// board build checks the real image (`build.rs`) for its own count.
    #[test]
    fn tseg_fits_1024_cpus() {
        use fstart_smm::{
            CpuSmmLayout, SMM_IDENTITY_TABLE_SIZE, SMM_MAPPED_TABLES_MAX, SmramLayout,
        };
        let layout = SmramLayout {
            smram_base: 0x7f00_0000,
            smram_size: TSEG_SIZE.into(),
            entry_count: 1024,
            save_state_size: 0x400,
            stack_size: crate::host::SMM_STACK_SIZE,
            entry_stub_size: 0x400,
            handler_mem_size: 4 << 20,
            // The ECAM and PCI windows above 4 GiB (`smm::smm_reach`).
            page_table_size: SMM_IDENTITY_TABLE_SIZE + SMM_MAPPED_TABLES_MAX,
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
