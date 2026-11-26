// SPDX-License-Identifier: MIT OR Apache-2.0
//! The SP7 (Venice / WH) SMM handler: PSP `SmmInfo` registration, optional
//! `RasEinj::ENABLE`, PSP P2C SMI service, and per-CPU SMCA RAS thresholding
//! and BMC SEL delivery ([`crate::smm::ras`]).
//!
//! Layout and protocol match `AmdPspSmmV2` / `AmdPlatformRasWhDxe` on Venice:
//! - SW SMI `0xD0` (`PSP_SMM_INFO`) sends `SmmInfo` and, if `config.einj` is
//!   set, `RasEinj::ENABLE` over the PSP C2P mailbox (`MP0_C2PMSG_31` opens
//!   only after `RasEinj::ENABLE` with the SMM flag).
//! - PSP P2C SMI (`source::PSP`) answers PSP requests by the WH PSP's rules
//!   (`psp::p2c::answer_of`) and releases the lock.
//! - SW SMI `0xD2` (`fch::smi::sw::FCH_LOCK`, raised at ready-to-boot)
//!   sends `LockFCHReg` (C2P `30h`) with the SMM flag, as FchSmmInit's
//!   SmmReadyToBoot does (Dell `0x9550`/`0x97d3`, GIGABYTE `0x52db`:
//!   `PcdFchOemSecureEnable` 1 in both donors). `LockGpio` (`3Ah`) is not
//!   sent: `PcdFchOemGpioFencing` is 0 in both.
//! - The OS's SLP_TYP write to S5 traps to SMM, which finishes the entry as
//!   the vendor's FCH SMM dispatcher does: SLP_TYP without SLP_EN, then the
//!   SMU v14 sleep entry (`fch::qilian::sx::enter_s5_posted`).
//! - Each CPU entering SMM runs `ras::enter` (supporting up to 1024 SP7
//!   threads); the rendezvous owner runs `ras::own` (storm control + SEL).
//! - Firmware-first PCIe/CXL errors ([`Config::firmware_first`],
//!   FIRMWARE_FIRST.md §2.1-§2.2): the IOHCs' RAS SMI (SmiStat1\[21\]) runs
//!   `smm::aer` on the root ports of every root bridge whose
//!   `RAS_GLOBAL_STATUS_HI` names a PCIe error (and ends a CXL event of
//!   `RAS_GLOBAL_STATUS_LO`), records into the GHES blocks, clears the
//!   PCIe status, then the FCH's (W1C) and re-arms every IOHC, as AGESA's
//!   dispatcher; then the SCI (software SCI event, CPMRAS `_L14`) or NMI
//!   (FakeSts0) the GHESs are notified with, and the errors to the BMC.
//!   More SMIs than the storm bucket holds (`Policy::storm`, the vendors'
//!   5 per second) turn the SMI off and poll on the periodic SMI until a
//!   quiet bucket's time has passed.
//! - What the handler reaches for the OS (the WSMT's
//!   `COMM_BUFFER_NESTED_PTR_PROTECTION`): config space through the
//!   installer's ECAM window, every segment, mapped in its page tables
//!   when above 4 GiB ([`smm_reach`], `Smi::mapped`); a CXL register
//!   block, whose address the OS's BAR gives, only wholly inside a PCI
//!   window of its own root bridge as the DF plan gave it
//!   ([`Config::windows`]) and outside SMRAM, the report and the GHES
//!   blocks (`smm::aer::Reach`), its registers bounded to the block; the
//!   installer's report, GHES blocks and PSP BAR only below 4 GiB outside
//!   SMRAM. [`Aer`] is the AER mechanism every consumer takes: OS-first
//!   where the handler cannot reach the ECAM.

use fstart_driver_amd::fch::acpimmio::AcpiPorts;
use fstart_driver_amd::nbio::wh::init::{Policy as NbioPolicy, aer_mechanism};
use fstart_driver_amd::nbio::wh::ras::AerPath;
use fstart_pci::{PciWindow, PciWindowKind};
use fstart_smm::{MappedRange, Region};

/// The most root bridges the handler serves (SP7 2P: 2 sockets × 2 IODs ×
/// 4).
pub const RAS_ROOT_BRIDGES: usize = 16;

/// The most PCI windows of those root bridges the handler admits CXL
/// register blocks in ([`Config::windows`]): two each (below and above
/// 4 GiB) and the primary's second one below 4 GiB, with room to spare.
pub const RAS_WINDOWS: usize = 40;

/// A root bridge whose IOHC raises the RAS SMI: its root bus and IOHC, and
/// its PCI windows, `windows` of [`Config::windows`] from `first_window`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RasRootBridge {
    pub segment: u16,
    pub bus: u8,
    pub iohc: u8,
    pub first_window: u8,
    pub windows: u8,
    pub _reserved: [u8; 2],
}

/// What the installer gives the SP7 SMM handler, in the SMM header (in
/// SMRAM, so the OS can change none of it).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    /// The Qilian FCH ACPI I/O ports (`AcpiPorts`).
    pub ports: AcpiPorts,
    /// Socket 0 die 0 FCH root bridge PSP MMIO BAR below 4 GiB.
    pub psp_bar: u32,
    /// Send `RasEinj::ENABLE` right after `SmmInfo` (for the ACPI EINJ table).
    pub einj: bool,
    /// True if host-side IPMI KCS I/O decode was established during early init.
    /// Passed across the stage-to-SMM ELF boundary so the SMM handler does not
    /// rely on a separate per-ELF static.
    pub kcs_decoded: bool,
    /// PCI segments of the ECAM window at [`Self::ecam_base`].
    pub ecam_segments: u16,
    /// Reserved DRAM address of [`Report`], written only by this handler.
    pub report: u64,
    /// The ECAM window the handler reaches config space through, every
    /// segment of it: below 4 GiB or mapped in the handler's page tables
    /// (`Smi::mapped`).
    pub ecam_base: u64,
    /// The firmware-first PCIe/CXL error sources;
    /// [`crate::smm::aer::FirmwareFirst::NONE`] for OS-first.
    pub firmware_first: crate::smm::aer::FirmwareFirst,
    /// The root bridges of the IOHC RAS SMI, the first
    /// [`Self::root_bridge_count`].
    pub root_bridges: [RasRootBridge; RAS_ROOT_BRIDGES],
    pub root_bridge_count: u8,
    pub window_count: u8,
    pub _reserved: [u8; 6],
    /// The root bridges' PCI windows as the DF plan gave them (what their
    /// allocators may give a BAR), the first [`Self::window_count`]: the
    /// only places the handler reaches a CXL register block in
    /// ([`crate::smm::aer::Reach`]).
    pub windows: [Region; RAS_WINDOWS],
}

impl Config {
    /// The PCI windows of `rb` (none for a malformed entry).
    pub fn windows_of(&self, rb: &RasRootBridge) -> &[Region] {
        windows_of(&self.windows, self.window_count, rb)
    }
}

/// The windows of `rb` among the first `count` of `windows`; none for a
/// malformed entry. Free of panics, for SMM.
fn windows_of<'a>(windows: &'a [Region], count: u8, rb: &RasRootBridge) -> &'a [Region] {
    let first = usize::from(rb.first_window);
    let end = first + usize::from(rb.windows);
    if end > usize::from(count) {
        return &[];
    }
    windows.get(first..end).unwrap_or(&[])
}

/// The AER mechanism in effect: the one source of every AER consumer on
/// SP7 (`sp7::ras` module doc). The NBIO policy's, OS-first when it asks
/// for firmware-first and the handler cannot run ([`Self::decide`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Aer {
    /// PCIe AER on (`PcdCfgAEREnable`, SMU message 9 argument 0).
    pub enabled: bool,
    /// `nbio::wh::init::aer_mechanism` (SMU message 9 argument 1, the ports' routing).
    pub mechanism: u8,
}

impl Aer {
    /// The mechanism of `policy`, OS-first if it is firmware-first and the
    /// SMM handler does not run (`handler`).
    pub const fn decide(policy: &NbioPolicy, handler: bool) -> Self {
        let mechanism = if policy.aer_mechanism == aer_mechanism::FIRMWARE_FIRST && !handler {
            aer_mechanism::OS_FIRST
        } else {
            policy.aer_mechanism
        };
        Self {
            enabled: policy.aer,
            mechanism,
        }
    }

    /// Whether the errors go to the SMM handler: the GHES sources, the
    /// IOHC RAS SMI, `_OSC` keeping AER and DPC.
    pub const fn firmware_first(&self) -> bool {
        self.enabled && self.mechanism == aer_mechanism::FIRMWARE_FIRST
    }

    /// SMU message 9's arguments.
    pub const fn smu_args(&self) -> [u32; 6] {
        [self.enabled as u32, self.mechanism as u32, 0, 0, 0, 0]
    }

    /// The trained ports' routing (Dell's interrupt redirect and sync
    /// flood on fatal: off), none without AER or a mechanism.
    pub const fn path(&self) -> Option<AerPath> {
        if self.enabled && self.mechanism != 0 {
            Some(AerPath::dell(self.mechanism))
        } else {
            None
        }
    }
}

/// Whether the SMM handler reaches the ECAM window `ecam`, every segment
/// of it: below 4 GiB, or mapped in its page tables (below 256 TiB,
/// `fstart_smm::MAPPED_LIMIT`). Extended config space (AER) needs it.
pub fn smm_reaches(ecam: &fstart_driver_amd::ccx::msr::EcamWindow) -> bool {
    fstart_smm::identity_table_size(&[Region::new(ecam.base, ecam.size())]).is_ok()
}

/// What the SMM handler reaches ([`smm_reach`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmmReach {
    pub root_bridges: [RasRootBridge; RAS_ROOT_BRIDGES],
    pub root_bridge_count: u8,
    /// The root bridges' PCI windows (`RasRootBridge::first_window`).
    pub windows: [Region; RAS_WINDOWS],
    pub window_count: u8,
    /// The ECAM window and those PCI windows above 4 GiB, for the page
    /// tables.
    pub mapped: heapless::Vec<MappedRange, { RAS_WINDOWS + 1 }>,
}

impl SmmReach {
    /// The PCI windows of `rb`, one of [`Self::root_bridges`].
    pub fn windows_of(&self, rb: &RasRootBridge) -> &[Region] {
        windows_of(&self.windows, self.window_count, rb)
    }
}

/// What the SMM handler reaches: the ECAM window `ecam` and, for each root
/// bridge of `roots` (its RAS SMI entry and its PCI root), the windows its
/// allocator gives BARs from, which bound the CXL register blocks the
/// handler follows (`smm::aer::Reach`). The ECAM and every window above
/// 4 GiB are mapped in the handler's page tables; if their tables exceed
/// the SMRAM the tables may take (`fstart_smm::SMM_MAPPED_TABLES_MAX`),
/// the windows above 4 GiB are left out (their CXL protocol errors are
/// then not logged, their PCIe errors are), and so is any window above
/// 256 TiB or beyond [`RAS_WINDOWS`].
pub fn smm_reach<'a>(
    ecam: Region,
    roots: impl Iterator<Item = (RasRootBridge, &'a [PciWindow])> + Clone,
) -> SmmReach {
    let build = |high: bool| {
        let mut reach = SmmReach {
            root_bridges: [RasRootBridge::default(); RAS_ROOT_BRIDGES],
            root_bridge_count: 0,
            windows: [Region::EMPTY; RAS_WINDOWS],
            window_count: 0,
            mapped: heapless::Vec::new(),
        };
        let above_4g = |r: &Region| r.end().is_none_or(|end| end > 1 << 32);
        if above_4g(&ecam) {
            let _ = reach.mapped.push(ecam);
        }
        for (slot, (mut rb, windows)) in reach.root_bridges.iter_mut().zip(roots.clone()) {
            rb.first_window = reach.window_count;
            rb.windows = 0;
            for w in windows.iter().filter(|w| w.kind == PciWindowKind::Mmio) {
                let region = Region::new(w.base, w.size);
                let mappable = region
                    .end()
                    .is_some_and(|end| end <= fstart_smm::MAPPED_LIMIT);
                let index = usize::from(reach.window_count);
                if !mappable || (above_4g(&region) && !high) || index >= RAS_WINDOWS {
                    continue;
                }
                if above_4g(&region) {
                    let _ = reach.mapped.push(region);
                }
                reach.windows[index] = region;
                reach.window_count += 1;
                rb.windows += 1;
            }
            *slot = rb;
            reach.root_bridge_count += 1;
        }
        reach
    };
    let reach = build(true);
    if fstart_smm::identity_table_size(&reach.mapped).is_ok() {
        return reach;
    }
    build(false)
}

/// What the SP7 SMM handler writes back to boot code (and keeps for diagnostics).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Report {
    pub smm_info: u32,
    pub einj: u32,
    /// Count of PSP P2C SMIs answered, and the last P2C command code.
    pub p2c_requests: u32,
    pub p2c_command: u32,
    /// `LockFCHReg`'s status (`crate::smm::status`).
    pub fch_lock: u32,
    /// Firmware-first: RAS SMIs and polls handled, records written,
    /// records a full block left out, storms.
    pub aer_passes: u32,
    pub aer_records: u32,
    pub aer_dropped: u32,
    pub aer_storms: u32,
}

impl Report {
    pub const NONE: Self = Self {
        smm_info: crate::smm::status::NOT_SENT,
        einj: crate::smm::status::NOT_SENT,
        p2c_requests: 0,
        p2c_command: 0,
        fch_lock: crate::smm::status::NOT_SENT,
        aer_passes: 0,
        aer_records: 0,
        aer_dropped: 0,
        aer_storms: 0,
    };
}

/// FCH SMI controller during TSEG installation: disables SMIs while relocating
/// SMBASE, then enables only the SMI command port and the PSP SMI.
#[cfg(feature = "bundle")]
pub struct Smi<'a, H> {
    pub fch: &'a H,
    pub config: Config,
    /// What the handler reaches above 4 GiB: the ECAM window and the PCI
    /// windows of [`Config::windows`] there (`sp7::ras::smm_reach`).
    pub mapped: heapless::Vec<MappedRange, { RAS_WINDOWS + 1 }>,
}

#[cfg(feature = "bundle")]
impl<H: fstart_driver_amd::fch::FchHw> fstart_arch::x86::cpu::amd::smm::SmiControl for Smi<'_, H> {
    type HandlerConfig = Config;

    fn smm_handler_config(&self) -> Config {
        self.config
    }

    fn smm_mapped_ranges(&self) -> &[MappedRange] {
        &self.mapped
    }

    fn disable_smi(&self) {
        fstart_driver_amd::fch::smi::disable(self.fch.acpi());
    }

    fn enable_smi(&self) {
        fstart_driver_amd::fch::smi::route_sleep(self.fch.acpi());
        fstart_driver_amd::fch::smi::enable_psp(self.fch.acpi());
        // The IOHCs' RAS SMI and the SCI its handler raises; the IOHCs
        // themselves are enabled once their ports are set up
        // (`sp7::ras::firmware_first`).
        let ff = &self.config.firmware_first;
        if ff.enabled() {
            fstart_driver_amd::fch::smi::route_nbio_ras(self.fch.acpi());
            if ff.sci() {
                fstart_driver_amd::fch::smi::software_sci_enable(self.fch.acpi(), ff.sci_event);
            }
        }
    }
}

#[cfg(feature = "smm")]
pub use handler::{enter_cpu, handle, handle_with};

#[cfg(feature = "smm")]
mod handler {
    use super::{Config, Report};
    use core::cell::Cell;

    use crate::smm::aer::PcieEvent;
    use crate::smm::ras::{self, Delivery, Event, Policy};
    use crate::smm::{SmmFch, psp as smm_psp, sw};
    use fstart_core::mmio::{read32, write32};
    use fstart_driver_amd::ccx::tsc::Tsc;
    use fstart_driver_amd::fch::FchHw;
    use fstart_driver_amd::fch::smi::{self, Pending, source};
    use fstart_driver_amd::psp::Variant;
    use fstart_driver_amd::psp::command::SmmInfo;
    use fstart_driver_amd::psp::einj::RasEinj;
    use fstart_driver_amd::psp::mailbox::{Mailbox, MmioTransport};
    use fstart_smm::{Region, SmmContext};

    /// Per-CPU SMM entry hook before rendezvous: arms/reads SMCA RAS state
    /// according to `policy` (up to 1024 SP7 logical CPUs, 2P).
    ///
    /// `Silicon::Gn` only keeps PFEH off Naples B1 there (`ccx::ras::arm`);
    /// the Zen 6 PFEH, MCE-redirect and SMI-trigger MSRs are the ones it
    /// programs (AmdPlatformRasWhSmm `0x16940`, `0x114a0`).
    #[inline(always)]
    pub fn enter_cpu(ctx: &mut SmmContext<'_>, _: &Config, policy: &Policy) {
        ras::enter(policy, ctx.cpu(), fstart_driver_amd::cpu::Silicon::Gn);
    }

    /// SP7 SMI handler on the rendezvous owner for boards using OS-first MCA.
    pub fn handle(ctx: &mut SmmContext<'_>, config: &Config) {
        handle_with(ctx, config, &Policy::OS_FIRST, ras::no_bmc);
    }

    /// SP7 SMI handler on the rendezvous owner: drains the RAS journal to
    /// `bmc` under storm control, then dispatches SW SMI `0xD0`, PSP P2C
    /// and the IOHCs' RAS SMI (firmware-first errors, whose events go to
    /// `bmc` too: on the OS's channel only with every CPU held).
    #[inline(always)]
    pub fn handle_with(
        ctx: &mut SmmContext<'_>,
        config: &Config,
        policy: &Policy,
        mut bmc: impl FnMut(&Event<'_>, Tsc) -> Delivery,
    ) {
        let smram = ctx.smram();
        crate::smm::with_fch_in(config.ecam_base, config.ecam_segments, |fch| {
            let first = ctx.first_owner();
            ras::own(policy, fch.acpi(), first, || ctx.cpus_held(), &mut bmc);
            let ff = &config.firmware_first;
            let handler = Handler {
                fch,
                config,
                policy,
                events: Cell::new([None; firmware_first::EVENTS]),
                smram,
                protected: [
                    smram,
                    Region::new(config.report, size_of::<Report>() as u64),
                    Region::new(ff.corrected.block, ff.corrected.length.into()),
                    Region::new(ff.uncorrected.block, ff.uncorrected.length.into()),
                ],
            };
            smi::dispatch(fch.acpi(), fch.io(), &config.ports, &handler);
            firmware_first::after_dispatch(&handler);
            let held = ctx
                .cpus_held()
                .is_some_and(|(inside, total)| inside >= total);
            if !policy.shared_bmc || held {
                firmware_first::deliver(&handler, &mut bmc);
            }
            // Release the PSP P2C lock after handling pending SMIs.
            smm_psp::release();
        });
    }

    struct Handler<'a> {
        fch: &'a SmmFch,
        config: &'a Config,
        policy: &'a Policy,
        /// This SMI's errors for the BMC.
        events: Cell<[Option<PcieEvent>; firmware_first::EVENTS]>,
        smram: Region,
        /// What no address the OS names may reach: SMRAM, the report and
        /// the GHES blocks, which the handler writes.
        protected: [Region; 4],
    }

    impl Handler<'_> {
        /// The report, if it is where the installer may put it: below
        /// 4 GiB, outside SMRAM.
        fn report(&self) -> Option<*mut Report> {
            outside_smram(self.smram, self.config.report, size_of::<Report>() as u64)
                .then_some(self.config.report as *mut Report)
        }

        /// The PSP's mailbox, at a BAR outside SMRAM.
        fn mailbox(&self) -> Option<Mailbox<MmioTransport>> {
            let bar = u64::from(self.config.psp_bar);
            // SAFETY: the PSP BAR mapped below 4 GiB on die 0 RB 0 during
            // cold init, outside SMRAM.
            outside_smram(self.smram, bar, crate::sp7::fabric::PSP_BAR_SIZE)
                .then(|| unsafe { Mailbox::mmio(bar, Variant::Wh) })
        }
    }

    /// Whether `[base, base + size)` is non-empty memory below the 4 GiB
    /// every SMM image maps, outside SMRAM: what the installer's addresses
    /// in [`Config`] must be.
    fn outside_smram(smram: Region, base: u64, size: u64) -> bool {
        base != 0 && fstart_smm::buffer::permitted(base, size, &[Region::new(0, 1 << 32)], &[smram])
    }

    impl smi::Handler for Handler<'_> {
        fn command(&self, command: u8) {
            let (Some(mailbox), Some(report)) = (self.mailbox(), self.report()) else {
                return;
            };
            if command == sw::PSP_SMM_INFO {
                psp(self.config, &mailbox, &self.fch.timer, report);
            } else if command == smi::sw::FCH_LOCK {
                fch_lock(&mailbox, &self.fch.timer, report);
            }
        }

        fn sources(&self, pending: &Pending) {
            if self.config.firmware_first.enabled() && pending.has(source::NBIO_RAS) {
                // Its status is cleared after the handler's work, then the
                // IOHCs re-armed (the vendor's dispatcher order).
                pending.acknowledged(source::NBIO_RAS);
                firmware_first::smi(self);
            }
            if pending.has(source::PSP) {
                let command = smm_psp::answer_of(self.fch.acpi(), pending, Variant::Wh);
                let Some(report) = self.report() else {
                    return;
                };
                // SAFETY: reserved DRAM report owned exclusively by this handler.
                unsafe {
                    let requests = &raw mut (*report).p2c_requests;
                    write32(requests, read32(requests).wrapping_add(1));
                    write32(&raw mut (*report).p2c_command, command);
                }
            }
        }

        /// OS owns ACPI directly.
        fn power_button(&self) {}

        /// S5 as the vendor's FCH SMM dispatcher enters it; other types
        /// return (SP7 publishes S0 and S5 only).
        fn sleep(&self, slp_typ: u8) {
            fstart_driver_amd::fch::qilian::sx::enter_s5_posted(
                self.fch,
                &self.config.ports,
                slp_typ,
            );
        }
    }

    /// The firmware-first glue of the SP7 handler: the GHES blocks in
    /// reserved memory, config space and the IOHCs through `crate::smm::Ecam`,
    /// the CXL register blocks below the 4 GiB the handler maps, the storm.
    mod firmware_first {
        use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

        use super::{Delivery, Event, Handler, PcieEvent, Region, outside_smram};
        use crate::smm::Ecam;
        use crate::smm::aer::{Blocks, Ghes, Logged, Pass, Reach, root_ports};
        use crate::smm::ras::{Bucket, POLL_REQUEST, TryLock};
        use fstart_core::mmio::{read32, write32};
        use fstart_core::services::Timer;
        use fstart_driver_amd::ccx::tsc::Tsc;
        use fstart_driver_amd::fch::FchHw;
        use fstart_driver_amd::fch::smi::{self, Route, source};
        use fstart_driver_amd::nbio::wh::RootBridge;
        use fstart_driver_amd::smn::SMN_INDEX;
        use fstart_pci::ConfigRegionAccess;
        use fstart_pci::aer::Errors;

        /// Errors of one SMI for the BMC; more are counted, not sent.
        pub(super) const EVENTS: usize = 4;
        /// Errors sent to the BMC per boot.
        const EVENTS_PER_BOOT: u32 = 64;

        static BUCKET: TryLock<Bucket> = TryLock::new(Bucket::EMPTY);
        /// The storm: the RAS SMI is off, every SMI polls.
        static POLLING: AtomicBool = AtomicBool::new(false);
        /// The last poll that found an error, or the storm's start.
        static ACTIVE_US: AtomicU64 = AtomicU64::new(0);
        /// This handler started the periodic SMI (no machine-check storm
        /// clock runs it).
        static PERIODIC: AtomicBool = AtomicBool::new(false);
        static SENT: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

        /// A GHES's block, if it is below 4 GiB and outside SMRAM.
        ///
        /// # Safety
        /// `ghes` is the installer's (in SMRAM, trusted): reserved DRAM,
        /// written only by this handler (and the OS's block status clear),
        /// whose contents [`fstart_acpi::cper::append`] bounds.
        unsafe fn block<'a>(ghes: &Ghes, smram: Region) -> Option<&'a mut [u8]> {
            let length = usize::try_from(ghes.length).ok()?;
            (length >= 20 && outside_smram(smram, ghes.block, ghes.length.into()))
                .then(|| unsafe { core::slice::from_raw_parts_mut(ghes.block as *mut u8, length) })
        }

        /// Add `n` to a counter of the report, `field` its offset there.
        fn count(
            handler: &Handler<'_>,
            field: impl FnOnce(*mut super::Report) -> *mut u32,
            n: u32,
        ) {
            if let Some(report) = handler.report() {
                let field = field(report);
                // SAFETY: the reserved report, written only by this handler.
                unsafe { write32(field, read32(field).wrapping_add(n)) }
            }
        }

        /// One pass over every root bridge's IOHC RAS status.
        fn pass(handler: &Handler<'_>) -> Logged {
            let config = handler.config;
            let ff = &config.firmware_first;
            // SAFETY: the installer's blocks (`block`).
            let (Some(corrected), Some(uncorrected)) =
                (unsafe { block(&ff.corrected, handler.smram) }, unsafe {
                    block(&ff.uncorrected, handler.smram)
                })
            else {
                return Logged::default();
            };
            let mut blocks = Blocks {
                corrected,
                uncorrected,
            };
            // SAFETY: SMM owns config space while it runs.
            let errors = unsafe { Errors::new(Ecam) };
            let mut logged = Logged::default();
            let mut record = |event: &PcieEvent| {
                let mut events = handler.events.get();
                if let Some(slot) = events.iter_mut().find(|slot| slot.is_none()) {
                    *slot = Some(*event);
                }
                handler.events.set(events);
            };
            let count = usize::from(config.root_bridge_count);
            for rb in config.root_bridges.iter().take(count) {
                let root = fstart_pci::address(rb.segment, rb.bus, 0);
                // The OS may be in the middle of an SMN access on this
                // root bus: its index is restored.
                // SAFETY: SMM owns the CPU and config space.
                let index = unsafe { Ecam.read(root, SMN_INDEX) };
                // SAFETY: the installer's root bus; its SMN pair is SMM's
                // until the index is restored below.
                if let Some(nbio) = unsafe { RootBridge::on_root(Ecam, rb.iohc, root) } {
                    let status = nbio.ras_status();
                    if !status.idle() {
                        if status.pcie() != 0 {
                            // The CXL register blocks behind this root
                            // bridge: in its PCI windows, outside SMRAM,
                            // the report and the blocks.
                            let reach = Reach {
                                windows: config.windows_of(rb),
                                protected: &handler.protected,
                            };
                            let mut pass = Pass::new(
                                &errors,
                                |base| reach.block(base),
                                &mut blocks,
                                &mut record,
                            );
                            root_ports(&errors, root, |port| {
                                pass.root_port(port);
                            });
                            logged.add(pass.logged);
                            nbio.clear_pcie_ras(status.pcie());
                        }
                        if status.cxl() {
                            nbio.take_cxl_event();
                        }
                    }
                }
                // SAFETY: as above.
                unsafe { Ecam.write(root, SMN_INDEX, index) };
            }
            logged
        }

        /// Re-arm every root bridge's IOHC RAS SMI.
        fn rearm(handler: &Handler<'_>) {
            let config = handler.config;
            for rb in config
                .root_bridges
                .iter()
                .take(usize::from(config.root_bridge_count))
            {
                let root = fstart_pci::address(rb.segment, rb.bus, 0);
                // SAFETY: as in `pass`.
                let index = unsafe { Ecam.read(root, SMN_INDEX) };
                // SAFETY: as in `pass`.
                if let Some(nbio) = unsafe { RootBridge::on_root(Ecam, rb.iohc, root) } {
                    nbio.rearm_ras_smi();
                }
                // SAFETY: as above.
                unsafe { Ecam.write(root, SMN_INDEX, index) };
            }
        }

        /// Tell the OS what `logged` put in the blocks; count it.
        fn signal(handler: &Handler<'_>, logged: &Logged) {
            let ff = &handler.config.firmware_first;
            let acpi = handler.fch.acpi();
            let (sci, nmi) = logged.signals(ff);
            if sci {
                smi::software_sci(acpi, ff.sci_event);
            }
            if nmi {
                smi::nmi(acpi);
            }
            // SAFETY (each): a field of the checked report.
            count(handler, |r| unsafe { &raw mut (*r).aer_passes }, 1);
            count(
                handler,
                |r| unsafe { &raw mut (*r).aer_records },
                u32::from(logged.corrected) + u32::from(logged.uncorrected),
            );
            count(
                handler,
                |r| unsafe { &raw mut (*r).aer_dropped },
                logged.dropped.into(),
            );
        }

        fn now_us() -> u64 {
            // SAFETY: SMM, ring 0; the P-state MSRs.
            Tsc::native(&unsafe { fstart_arch::x86::cpu::msr::NativeMsr::new() }).timestamp_us()
        }

        /// The IOHCs' RAS SMI: a pass, the FCH's status, the re-arm, the
        /// notifications; more than the storm bucket holds turns the SMI
        /// off and polls ([`after_dispatch`]).
        // The statics stay in this crate (no GOT in the board's SMM image).
        #[inline(never)]
        pub(super) fn smi(handler: &Handler<'_>) {
            let logged = pass(handler);
            smi::clear(handler.fch.acpi(), source::NBIO_RAS);
            rearm(handler);
            signal(handler, &logged);
            let storm = &handler.policy.storm;
            let now = now_us();
            if BUCKET.with(|bucket| bucket.hit(storm, now)) == Some(true) {
                smi::set_route(handler.fch.acpi(), source::NBIO_RAS, Route::Disabled);
                ACTIVE_US.store(now, Ordering::Relaxed);
                POLLING.store(true, Ordering::Release);
                // SAFETY: a field of the checked report.
                count(handler, |r| unsafe { &raw mut (*r).aer_storms }, 1);
                if handler.policy.smm() {
                    POLL_REQUEST.store(true, Ordering::Release);
                } else {
                    PERIODIC.store(true, Ordering::Relaxed);
                    smi::periodic(handler.fch.acpi(), Some(storm.poll_ms));
                }
            }
        }

        /// In a storm, every SMI polls; a bucket's time without an error
        /// ends it (the RAS SMI routed again, the periodic SMI stopped).
        #[inline(never)]
        pub(super) fn after_dispatch(handler: &Handler<'_>) {
            if !handler.config.firmware_first.enabled() || !POLLING.load(Ordering::Acquire) {
                return;
            }
            let logged = pass(handler);
            let now = now_us();
            if logged.corrected != 0 || logged.uncorrected != 0 {
                ACTIVE_US.store(now, Ordering::Relaxed);
                signal(handler, &logged);
                rearm(handler);
                return;
            }
            let window = u64::from(handler.policy.storm.window_ms) * 1000;
            if now.wrapping_sub(ACTIVE_US.load(Ordering::Relaxed)) < window {
                return;
            }
            POLLING.store(false, Ordering::Release);
            POLL_REQUEST.store(false, Ordering::Release);
            if PERIODIC.swap(false, Ordering::Relaxed) {
                smi::periodic(handler.fch.acpi(), None);
            }
            rearm(handler);
            smi::route_nbio_ras(handler.fch.acpi());
        }

        /// This SMI's errors to the BMC, a budget per boot; the first
        /// failure ends them.
        pub(super) fn deliver(
            handler: &Handler<'_>,
            bmc: &mut impl FnMut(&Event<'_>, Tsc) -> Delivery,
        ) {
            let events = handler.events.take();
            if events.iter().all(Option::is_none) {
                return;
            }
            // SAFETY: SMM, ring 0; the P-state MSRs.
            let timer = Tsc::native(&unsafe { fstart_arch::x86::cpu::msr::NativeMsr::new() });
            for event in events.iter().flatten() {
                if !budget() {
                    return;
                }
                let delivery = bmc(&Event::Pcie(event), timer);
                if !spent(delivery) {
                    return;
                }
            }
        }

        /// Whether the boot's budget of events has room.
        #[inline(never)]
        fn budget() -> bool {
            SENT.load(Ordering::Relaxed) < EVENTS_PER_BOOT
        }

        /// Count what the BMC did with an event; whether to go on.
        #[inline(never)]
        fn spent(delivery: Delivery) -> bool {
            match delivery {
                Delivery::Sent => {
                    SENT.fetch_add(1, Ordering::Relaxed);
                    true
                }
                Delivery::Busy => false,
                Delivery::Failed => {
                    SENT.store(EVENTS_PER_BOOT, Ordering::Relaxed);
                    false
                }
            }
        }
    }

    /// `LockFCHReg` with the SMM flag set, through the C2P page `SmmInfo`
    /// registered.
    #[inline(never)]
    fn fch_lock(mailbox: &Mailbox<MmioTransport>, timer: &Tsc, report: *mut Report) {
        use fstart_driver_amd::psp::command::LockFchReg;
        // SAFETY: the rendezvous owner; SmmInfo registered the buffer.
        let locked = unsafe { smm_psp::send(mailbox, timer, LockFchReg) };
        // SAFETY: as in `sources`.
        unsafe { write32(&raw mut (*report).fch_lock, crate::smm::status::of(locked)) };
    }

    /// Register `SmmInfo` (zeroing reserved descriptors) and, when enabled,
    /// send `RasEinj::ENABLE` with the SMM flag. Runs once per boot.
    #[inline(never)]
    fn psp(config: &Config, mailbox: &Mailbox<MmioTransport>, timer: &Tsc, report: *mut Report) {
        let Some(registered) = smm_psp::register(mailbox, timer, |info| SmmInfo {
            reserved: Default::default(),
            ..info
        }) else {
            return;
        };
        // SAFETY: as in `sources`.
        unsafe {
            write32(
                &raw mut (*report).smm_info,
                crate::smm::status::of(registered),
            )
        };
        if registered.is_err() || !config.einj {
            return;
        }
        // SAFETY: SmmInfo registered the C2P SMM buffer; rendezvous owner only.
        let enabled = unsafe { smm_psp::send(mailbox, timer, RasEinj::ENABLE) };
        // SAFETY: as in `sources`.
        unsafe { write32(&raw mut (*report).einj, crate::smm::status::of(enabled)) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_handler_config_fits_the_smm_header() {
        assert!(core::mem::size_of::<Config>() <= fstart_smm::HANDLER_CONFIG_CAPACITY);
    }

    use fstart_driver_amd::ccx::msr::EcamWindow;
    use fstart_driver_amd::df::{BusRange, plan, wh as df_wh};
    use fstart_pci::PciRootInfo;
    use std::vec::Vec;

    /// The MD25-CG0's ECAM: 256 MiB at 0x2FFC_8000_0000 (its APCB tokens).
    const MD25_ECAM: EcamWindow = match EcamWindow::new(0x2ffc_8000_0000, 1) {
        Some(window) => window,
        None => panic!(),
    };

    /// The ECAM above 4 GiB is mapped in the handler's tables, so the
    /// NBIO policy's firmware-first stays: SMU message 9, the ports'
    /// routing and the tables (`firmware_first`) all say mechanism 2.
    #[test]
    fn an_ecam_above_4g_keeps_firmware_first() {
        let policy = NbioPolicy::default();
        for ecam in [
            MD25_ECAM,
            super::super::DEFAULT_ECAM,
            EcamWindow::new(0x1000_0000_0000, 2).unwrap(),
        ] {
            assert!(smm_reaches(&ecam), "{:#x}", ecam.base);
            let aer = Aer::decide(&policy, smm_reaches(&ecam));
            assert!(aer.firmware_first());
            assert_eq!(aer.smu_args(), [1, 2, 0, 0, 0, 0]);
            assert_eq!(
                aer.path(),
                Some(AerPath::dell(aer_mechanism::FIRMWARE_FIRST))
            );
        }
    }

    /// A handler that cannot run (an ECAM the 4-level tables cannot map,
    /// or forced off) makes every consumer OS-first: SMU message 9, the
    /// ports' routing, no firmware-first sources (so no GHES, no AERR/_L14,
    /// `_OSC` grants AER and DPC). An OS-first or AER-off policy stays.
    #[test]
    fn a_handler_that_cannot_run_is_os_first_everywhere() {
        let policy = NbioPolicy::default();
        let unreachable = EcamWindow::new(1 << 48, 1).unwrap();
        assert!(!smm_reaches(&unreachable));
        for handler in [smm_reaches(&unreachable), false] {
            let aer = Aer::decide(&policy, handler);
            assert!(!aer.firmware_first());
            assert_eq!(aer.mechanism, aer_mechanism::OS_FIRST);
            assert_eq!(aer.smu_args(), [1, 1, 0, 0, 0, 0]);
            assert_eq!(aer.path(), Some(AerPath::dell(aer_mechanism::OS_FIRST)));
        }
        let os_first = NbioPolicy {
            aer_mechanism: aer_mechanism::OS_FIRST,
            ..policy
        };
        assert_eq!(
            Aer::decide(&os_first, true).mechanism,
            aer_mechanism::OS_FIRST
        );
        let off = NbioPolicy {
            aer: false,
            ..policy
        };
        let aer = Aer::decide(&off, true);
        assert!(!aer.firmware_first());
        assert_eq!((aer.smu_args()[0], aer.path()), (0, None));
    }

    /// A 2P B0 plan (12 root bridges) with the MD25's ECAM above 4 GiB.
    fn plan_2p(ecam: EcamWindow) -> crate::sp7::fabric::Plan {
        let fids = [
            0x20u16, 0x21, 0x22, 0x60, 0x61, 0x62, 0xa0, 0xa1, 0xa2, 0xe0, 0xe1, 0xe2,
        ];
        let routes: Vec<_> = (fids.iter().enumerate())
            .map(|(i, &destination)| df_wh::BusRoute {
                segment: 0,
                buses: BusRange {
                    base: i as u8 * 21,
                    limit: i as u8 * 21 + 20,
                },
                destination,
            })
            .collect();
        let ios: Vec<df_wh::Ios> = (0..2)
            .flat_map(|die| crate::sp7::fabric::assumed_ios(&df_wh::devices::B0, die))
            .collect();
        let nodes = [(0, 0), (0, 1), (1, 0), (1, 1)];
        let roots = crate::sp7::fabric::resolve_roots(&routes, &ios, &nodes, 1).unwrap();
        plan::plan_for(
            &plan::Input {
                tom: 0x8000_0000,
                tom2: 0x100_0000_0000,
                ecam_base: ecam.base,
                ecam_size: ecam.size(),
                phys_addr_bits: 52,
                root_bridges: &roots,
                mmio_pairs: df_wh::MMIO_PAIRS,
            },
            &plan::Policy::df3(),
        )
        .unwrap()
    }

    /// What the handler reaches on a 2P system with the MD25's ECAM: each
    /// root bridge its own PCI windows (never its non-PCI pool, where the
    /// PSP, IOMMU and IOAPIC BARs are, nor DRAM, TSEG or the ECAM), the
    /// ECAM and the windows above 4 GiB mapped, within the tables' SMRAM.
    #[test]
    fn the_handler_reaches_each_root_bridges_pci_windows() {
        let plan = plan_2p(MD25_ECAM);
        let first = PciRootInfo {
            segment: 0,
            ecam_base: MD25_ECAM.base,
            bus_start: 0,
            bus_end: 0xff,
        };
        let fabric = crate::soc::Fabric {
            segment: first,
            segments: 1,
            bridges: &plan.root_bridges,
        };
        let roots = fabric.roots().unwrap();
        let entries = plan.root_bridges.iter().zip(&roots).map(|(rb, root)| {
            let entry = RasRootBridge {
                segment: rb.segment,
                bus: rb.bus.base,
                iohc: rb.index,
                ..RasRootBridge::default()
            };
            (entry, root.allocatable())
        });
        let ecam = Region::new(MD25_ECAM.base, MD25_ECAM.size());
        let reach = smm_reach(ecam, entries);
        assert_eq!(usize::from(reach.root_bridge_count), 12);
        assert_eq!(reach.mapped[0], ecam);
        // The windows above 4 GiB fit the tables' reserve: none left out.
        assert!(reach.mapped.len() > 1);
        let tables = fstart_smm::identity_table_size(&reach.mapped).unwrap();
        assert!(tables > fstart_smm::SMM_IDENTITY_TABLE_SIZE);
        let config = Config {
            ports: AcpiPorts {
                event: 0,
                control: 0,
                timer: 0,
                gpe: 0,
            },
            psp_bar: 0,
            einj: false,
            kcs_decoded: false,
            ecam_segments: 1,
            report: 0,
            ecam_base: MD25_ECAM.base,
            firmware_first: crate::smm::aer::FirmwareFirst::NONE,
            root_bridges: reach.root_bridges,
            root_bridge_count: reach.root_bridge_count,
            window_count: reach.window_count,
            _reserved: [0; 6],
            windows: reach.windows,
        };
        let tseg = Region::new(0x7f00_0000, u64::from(super::super::TSEG_SIZE));
        for (rb, entry) in plan.root_bridges.iter().zip(&reach.root_bridges) {
            let windows = config.windows_of(entry);
            assert!(!windows.is_empty() && windows.len() <= 3);
            for w in windows {
                assert!(!w.overlaps(rb.below_pools.non_pci.base, rb.below_pools.non_pci.size));
                assert!(!w.overlaps(tseg.base, tseg.size));
                assert!(!w.overlaps(ecam.base, ecam.size));
                assert!(!w.overlaps(0, 0x8000_0000), "DRAM below 4 GiB");
                let mapped = w.end().unwrap() <= 1 << 32
                    || reach.mapped.iter().any(|m| m.contains(w.base, w.size));
                assert!(mapped, "{w:x?}");
            }
            // A register block in another root bridge's window is not
            // this one's.
            let others = plan
                .root_bridges
                .iter()
                .filter(|o| o.index != rb.index || o.socket != rb.socket);
            for other in others {
                let pool = other.below_pools.non_prefetch;
                let reach = crate::smm::aer::Reach {
                    windows,
                    protected: &[tseg],
                };
                assert!(reach.block(pool.base).is_none());
            }
        }
        // A malformed entry reaches nothing.
        let bad = RasRootBridge {
            first_window: reach.window_count,
            windows: 1,
            ..RasRootBridge::default()
        };
        assert!(config.windows_of(&bad).is_empty());
    }

    /// Windows whose tables exceed the SMRAM reserve are left out, the
    /// ECAM kept; a window above 256 TiB is never admitted.
    #[test]
    fn windows_beyond_the_table_reserve_are_left_out() {
        use fstart_pci::PciWindow;
        // 20 windows across a 512 GiB boundary each: two PDPTs and two
        // page directories apiece, 80 pages, more than the reserve.
        let windows: Vec<PciWindow> = (1..=20u64)
            .map(|i| PciWindow::mmio((i << 39) - (1 << 20), 3 << 20))
            .chain([
                PciWindow::mmio(0xe000_0000, 0x1000_0000),
                PciWindow::mmio(1 << 48, 1 << 30),
            ])
            .collect();
        let ecam = Region::new(MD25_ECAM.base, MD25_ECAM.size());
        let entries = [
            (RasRootBridge::default(), &windows[..10]),
            (RasRootBridge::default(), &windows[10..]),
        ];
        let reach = smm_reach(ecam, entries.iter().copied());
        assert_eq!(&reach.mapped[..], &[ecam]);
        assert_eq!(reach.window_count, 1);
        assert_eq!(reach.windows[0], Region::new(0xe000_0000, 0x1000_0000));
        assert_eq!(
            (
                reach.root_bridges[1].first_window,
                reach.root_bridges[1].windows
            ),
            (0, 1)
        );
        // Few enough: all of them but the one above 256 TiB.
        let entries = [(RasRootBridge::default(), &windows[10..])];
        let reach = smm_reach(ecam, entries.iter().copied());
        assert_eq!(reach.window_count, 11);
        assert_eq!(reach.mapped.len(), 11);
    }

    #[test]
    fn report_none_marks_both_psp_commands_unsent() {
        assert_eq!(Report::NONE.smm_info, crate::smm::status::NOT_SENT);
        assert_eq!(Report::NONE.einj, crate::smm::status::NOT_SENT);
        assert_eq!(Report::NONE.p2c_requests, 0);
        assert_eq!(Report::NONE.p2c_command, 0);
        assert_eq!(Report::NONE.fch_lock, crate::smm::status::NOT_SENT);
    }
}
