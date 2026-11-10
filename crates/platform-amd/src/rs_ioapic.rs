// SPDX-License-Identifier: MIT OR Apache-2.0
//! NBIO IOAPIC ownership on the Rs IOD: relocate each root's IOAPIC into the
//! upper half of its IOMMU's non-PCI slot, which `rs_iommu` already routes to
//! that root (one DF MMIO rule per root for both), route its bridges and
//! publish the ACPI routing.
//! openSIL NbioIoApic.c/NbioIoapicTbl.h through `nbio::ioapic::rs`, whose
//! `Plan` checks the CPU stepping it was validated on.
/// Claim one independent NBIO IOAPIC per live root after the CPU audit and
/// before any root-port bridge masters (AGESA does this in PEI, ahead of the
/// xHCI driver; a firmware-boot USB stack keeps its bridge mastering): ID
/// F1h + physical IOS, GSI base 24 + 32 x physical IOS (the numbering both
/// vendor firmwares publish).
/// # Safety
/// Checked stepping/global SMN relocation (proven by IOMMU policy), BSP-exclusive
/// DF/config/SMN, IF=0 on all CPUs and SMI disabled. No active INTx consumer.
pub unsafe fn initialize(
    roots: &[fstart_driver_amd::df::v4::Root],
    iommus: core::ops::Range<u64>,
    ids: &[u32],
) -> Result<alloc::vec::Vec<super::rs_acpi::RootInterrupts>, ()> {
    use fstart_driver_amd::{
        df::v4,
        mpio::ports,
        nbio::{
            ioapic::rs::{self as nbio_ioapic, Bridge, Target},
            iohc::NbProgDeviceRemap,
        },
        smn::Smn,
    };
    use fstart_pci::{EcamDevice, ecam::GlobalEcam};
    use tock_registers::interfaces::Readable;
    if roots.is_empty() || roots.len() > 4 {
        return Err(());
    }
    let mut targets = heapless::Vec::<Target, 4>::new();
    for (index, root) in roots.iter().enumerate() {
        let base = super::rs_resources::ioapic_mmio(&iommus, roots.len(), index).ok_or(())?;
        targets
            .push(Target {
                physical: root.ios.physical_index,
                base: u32::try_from(base).map_err(|_| ())?,
                id: 0xf1 + root.ios.physical_index,
            })
            .map_err(|_| ())?;
    }
    if targets.iter().any(|t| ids.contains(&u32::from(t.id))) {
        return Err(());
    }
    let smn = unsafe { Smn::on_bus(GlobalEcam, roots[0].route.base) };
    let plan = nbio_ioapic::Plan::prepare(
        &smn,
        fstart_arch::x86::mp::CpuIdentity::current().signature,
        &targets,
    )
    .map_err(|e| {
        fstart_log::error!("rs: IOAPIC preflight {:?}", e);
    })?;
    let mut result = alloc::vec::Vec::new();
    for root in roots {
        let physical = root.ios.physical_index;
        let mut bridges = alloc::vec::Vec::new();
        for slot in 0..24 {
            if physical & 1 != 0 && (18..22).contains(&slot) {
                continue;
            }
            let raw = smn
                .reg(ports::remap_register(physical, slot).map_err(|_| ())?)
                .extract();
            if raw.get() == u32::MAX {
                return Err(());
            }
            let devfn = raw.read(NbProgDeviceRemap::DEVFN) as u8;
            let device = EcamDevice::new(root.route.base, devfn >> 3, devfn & 7);
            if !device.is_present() || device.read8(fstart_pci::PCI_HEADER_TYPE) & 0x7f != 1 {
                continue;
            }
            let pin = device.read8(fstart_pci::PCI_INTERRUPT_PIN);
            if device.vendor_id() != 0x1022
                || pin > 4
                || device.read16(fstart_pci::PCI_COMMAND) & fstart_pci::PCI_CMD_BUS_MASTER != 0
                || bridges.iter().any(|p: &Bridge| p.devfn == devfn)
            {
                return Err(());
            }
            fstart_log::info!(
                "rs: IRQ bridge IOS={} slot={} devfn={:#x} pin={}",
                physical,
                slot,
                devfn,
                pin
            );
            bridges.push(Bridge {
                slot,
                devfn,
                interrupt_pin: pin,
            });
        }
        result.push(super::rs_acpi::RootInterrupts {
            ioapic: fstart_acpi::platform::IoApicConfig {
                id: 0xf1 + physical,
                base: targets
                    .iter()
                    .find(|t| t.physical == physical)
                    .ok_or(())?
                    .base
                    .into(),
                gsi_base: 24 + 32 * u32::from(physical),
            },
            bridges,
        });
    }
    // The IOMMU slot's DF rule must already route each IOAPIC to its root.
    let df = unsafe { v4::Df::new(GlobalEcam) };
    let table = v4::mmio::snapshot(&df).map_err(|_| ())?;
    for (root, target) in roots.iter().zip(&targets) {
        let base = u64::from(target.base);
        let routed = table.iter().any(|r| {
            matches!(
                v4::mmio::Route::decode(r.base, r.limit, r.control, r.extension),
                Ok(Some(route)) if route.base <= base
                    && base + nbio_ioapic::WINDOW_SIZE <= route.end
                    && route.destination == root.ios.fabric_id
                    && !route.cpu_disabled
            )
        });
        if !routed {
            fstart_log::error!(
                "rs: IOAPIC IOS={} base={:#x} is not routed by its IOMMU slot",
                target.physical,
                base
            );
            return Err(());
        }
    }
    unsafe {
        plan.install(&smn, |t| {
            let ioapic = fstart_arch::x86::ioapic::IoApic::new(t.base as usize);
            fstart_log::info!(
                "rs: IOAPIC MMIO IOS={} base={:#x} id={} pins={}",
                t.physical,
                t.base,
                ioapic.id(),
                ioapic.redirection_entries()
            );
            if ioapic.id() != t.id || ioapic.redirection_entries() != nbio_ioapic::PINS {
                return Err(nbio_ioapic::Error::Readback);
            }
            ioapic.mask_all();
            if !ioapic.all_masked() {
                return Err(nbio_ioapic::Error::Readback);
            }
            Ok(())
        })
    }
    .map_err(|e| {
        fstart_log::error!("rs: IOAPIC install {:?}; no handoff", e);
    })?;
    if unsafe { fstart_arch::x86::ioapic::IoApic::new(fstart_driver_amd::fch::IOAPIC as usize) }
        .id()
        != 0
    {
        return Err(());
    }
    Ok(result)
}
