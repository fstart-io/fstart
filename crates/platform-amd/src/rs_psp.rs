// SPDX-License-Identifier: MIT OR Apache-2.0
//! The primary PSP's MMIO window on the Rs IOD and its ACPI EINJ service.
//!
//! The PSP BAR is off at reset. It is IOS0's IOHC register (`13B102E0`,
//! offset `0 << 20`; AGESA's PSP base query), so the window must route to
//! IOS0, not to the FCH's primary IOS (through IOS2 it reads all-ones). The
//! vendor firmware maps it at `B9400000`; here it takes the slot after the
//! IOMMU slots ([`super::rs_resources::psp_mmio`]), routed by its own DF4
//! rule. SMM's C2P mailbox and the EINJ table's RAS mailbox
//! ([`fstart_driver_amd::psp::einj`]) use it.
use core::ops::Range;
use fstart_driver_amd::{
    apob::{rs::HOLE_RAS_EINJ, sysmap::Region},
    df::v4::{self, Root},
    nbio::iohc,
    psp::mailbox::C2P_MMIO_OFFSET,
    smn::Smn,
};
use fstart_pci::ecam::GlobalEcam;

/// The PSP window [`map`] routed and mapped; only [`map`] makes one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Psp {
    bar: u64,
}

/// The physical IOS whose IOHC holds the PSP BAR.
const PSP_IOS: u8 = 0;

/// Route `window` to the PSP's IOS and map the PSP BAR there, enabled and
/// locked; refuse a BAR something else already placed.
///
/// # Safety
/// BSP-exclusive DF4/SMN; `window` is the caller's reserved non-PCI MMIO,
/// identity mapped uncached.
pub unsafe fn map(roots: &[Root], window: Range<u64>) -> Result<Psp, ()> {
    let root = roots
        .iter()
        .find(|r| r.ios.physical_index == PSP_IOS)
        .ok_or(())?;
    if window.end.checked_sub(window.start) != Some(iohc::PSP_BAR_SIZE)
        || window.start % iohc::PSP_BAR_SIZE != 0
        || window.end > 1 << 32
    {
        return Err(());
    }
    // SAFETY: the caller's BSP-exclusive SMN.
    let smn = unsafe { Smn::on_bus(GlobalEcam, root.route.base) };
    if let Some(bar) = iohc::PSP_BAR.base(&smn)
        && bar != window.start
    {
        fstart_log::error!("rs: PSP BAR already at {:#x}", bar);
        return Err(());
    }
    // SAFETY: the caller's BSP-exclusive DF4 transport.
    let df = unsafe { v4::Df::new(GlobalEcam) };
    let table = v4::mmio::snapshot(&df).map_err(|_| ())?;
    let claim =
        v4::mmio::Claim::prepare(&table, window.start, iohc::PSP_BAR_SIZE, root.ios.fabric_id)
            .map_err(|_| ())?;
    let pair = claim.pair();
    // SAFETY: the caller's reserved window; Claim checks the rule is unused.
    unsafe { claim.install(&df) }.map_err(|_| ())?;
    iohc::map_psp(&smn, window.start as u32).map_err(|low| {
        fstart_log::error!("rs: PSP BAR locked low={:#x}", low);
    })?;
    if iohc::PSP_BAR.base(&smn) != Some(window.start) {
        return Err(());
    }
    // SAFETY: the BAR just mapped and routed.
    let c2p = unsafe { fstart_core::mmio::read32((window.start + C2P_MMIO_OFFSET) as *const u32) };
    fstart_log::info!(
        "rs: PSP BAR={:#x} DF-pair={} C2P={:#x}",
        window.start,
        pair,
        c2p
    );
    if c2p == u32::MAX {
        return Err(());
    }
    Ok(Psp { bar: window.start })
}

impl Psp {
    /// The PSP BAR's base address.
    pub const fn bar(&self) -> u64 {
        self.bar
    }

    /// The PSP's EINJ command buffer, which must be the ABL's
    /// `ReservedRasEinj` hole.
    pub fn einj_buffer(
        &self,
        regions: &[Region],
        timer: &impl fstart_core::services::timer::Timer,
    ) -> Result<u64, ()> {
        use fstart_driver_amd::{apob::sysmap::RegionKind, psp::einj};
        let hole = regions
            .iter()
            .find(|r| r.kind == RegionKind::Reserved(HOLE_RAS_EINJ))
            .ok_or(())?;
        // SAFETY: this BAR, mapped by `map`; the BSP is its only user.
        let mailbox = unsafe { einj::Mailbox::mmio(self.bar) };
        mailbox
            .buffer(timer, &(hole.base..hole.end()))
            .map_err(|error| {
                fstart_log::error!("rs: PSP EINJ buffer query {:?}", error);
            })
    }
}

#[cfg(feature = "rs-acpi")]
impl Psp {
    /// The PSP's EINJ service, once SMM opened it: the command buffer laid
    /// out as the vendor's `ApeiEinjInstall` leaves it for the EINJ table.
    pub fn einj(
        &self,
        regions: &[Region],
        timer: &impl fstart_core::services::timer::Timer,
    ) -> Result<super::rs_acpi::Einj, ()> {
        use fstart_driver_amd::psp::einj::{self, ERROR_TYPES};
        let base = self.einj_buffer(regions, timer)?;
        // SAFETY: the PSP's command buffer in the ABL's reserved hole,
        // identity mapped; the PSP serves no request before BEGIN.
        let buffer = unsafe { einj::Buffer::at(base) };
        let trigger = fstart_acpi::einj::trigger(&[einj::trigger(base)]);
        let types = buffer.prepare(&trigger).map_err(|error| {
            fstart_log::error!("rs: PSP EINJ buffer {:?}", error);
        })?;
        // Vendor types name DF F0 (00:18.0).
        if types.is_set(ERROR_TYPES::VENDOR) {
            let df = fstart_pci::EcamDevice::new(0, 0x18, 0).read32(0);
            buffer.describe_vendor_device(0x18, 0, df);
        }
        fstart_log::info!(
            "rs: PSP EINJ buffer={:#x} error-types={:#x}",
            base,
            types.get()
        );
        Ok(super::rs_acpi::Einj {
            mailbox: self.bar + einj::MAILBOX_OFFSET,
            buffer: base,
        })
    }
}
