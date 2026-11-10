// SPDX-License-Identifier: MIT OR Apache-2.0
//! Checked Rs IOMMU MMIO ownership/capture. No device tables, queues, L1/L2
//! policy, translation, interrupt remapping or security claims are installed.
use alloc::vec::Vec;
use core::ops::Range;
use fstart_driver_amd::{
    df::v4::{self, Root},
    nbio::{
        iohc,
        iommu::{self, Iommu},
    },
};
use fstart_pci::{ConfigRegister, EcamDevice, ecam::GlobalEcam};
use tock_registers::LocalRegisterCopy;

pub struct Mapped {
    pub root: Root,
    pub registers: Iommu,
}

/// Map only previously disabled, identity-checked IOMMUs into separately
/// reserved non-PCI windows, preserving all existing DF/USB/PCI apertures.
///
/// # Safety
/// The BSP owns DF/PCI configuration, has checked single-socket Rs identity and the live
/// root bindings. `window` is wholly inside a PSP MMIO hole, disjoint from
/// PCI/USB pools and fixed/reserved apertures, UC and identity mapped.
/// No other CPU or firmware path may change these BARs or routing.
pub unsafe fn initialize(
    roots: &[Root],
    window: Range<u64>,
    expected_device: u16,
) -> Result<Vec<Mapped>, ()> {
    let slot = super::rs_resources::IOMMU_SLOT_SIZE;
    if roots.is_empty()
        || roots.len() > v4::PHYSICAL_IOS.len()
        || window.end.checked_sub(window.start) != slot.checked_mul(roots.len() as u64)
        || window.start < 0x8000_0000
        || window.end > 0xe000_0000
        || window.start & (slot - 1) != 0
        || iommu::MMIO_SIZE > slot
    {
        return Err(());
    }
    // Preflight EVERY unit before any BAR/routing write, not one at a time
    // with implicit partial fallback when another unit differs.
    for (index, root) in roots.iter().enumerate() {
        if root.route.segment != 0
            || roots[..index].iter().any(|old| {
                old.route.base == root.route.base
                    || old.ios.physical_index == root.ios.physical_index
                    || old.ios.fabric_id == root.ios.fabric_id
            })
        {
            return Err(());
        }
        let device = EcamDevice::new(root.route.base, 0, 2);
        let header = LocalRegisterCopy::<u32, iommu::CapHeader::Register>::new(
            device.read32(iommu::CAPABILITY),
        );
        if device.vendor_id() != 0x1022
            || device.device_id() != expected_device
            || device.find_capability(iommu::CAPABILITY_ID as u8) != Some(iommu::CAPABILITY)
            || header.read(iommu::CapHeader::ID) != iommu::CAPABILITY_ID
            || device.read32(iommu::CAPABILITY + 4) != 0
            || device.read32(iommu::CAPABILITY + 8) != 0
        {
            fstart_log::error!(
                "rs: IOMMU preflight failed IOS={}; no BAR/routing write",
                root.ios.physical_index
            );
            return Err(());
        }
    }
    // SAFETY: checked node0, BSP-exclusive DF4 transport.
    let df = unsafe { v4::Df::new(GlobalEcam) };
    let pci = GlobalEcam;
    let mut mapped: Vec<Mapped> = Vec::new();
    for (index, root) in roots.iter().enumerate() {
        let base = window.start + index as u64 * slot;
        let table = v4::mmio::snapshot(&df).map_err(|_| ())?;
        let claim =
            v4::mmio::Claim::prepare(&table, base, slot, root.ios.fabric_id).map_err(|_| ())?;
        let pair = claim.pair();
        // SAFETY: caller's complete, disjoint PSP MMIO reservation. Claim
        // verifies unused-rule ownership and the full table's live readback.
        unsafe { claim.install(&df) }.map_err(|_| ())?;
        let device = EcamDevice::new(root.route.base, 0, 2);
        // SAFETY: preflighted AMD IOMMU capability in this exact function;
        // BSP-exclusive configuration writes. Use its real PCI registers,
        // not an unrelocated IOD-global SMN alias (Genoa openSIL uses PCI).
        let (low, high) = unsafe {
            (
                ConfigRegister::new(&pci, device.address(), iommu::CAPABILITY + 4),
                ConfigRegister::new(&pci, device.address(), iommu::CAPABILITY + 8),
            )
        };
        fstart_log::info!(
            "rs: IOMMU map IOS={} DF-pair={} base={:#x}",
            root.ios.physical_index,
            pair,
            base
        );
        iohc::IOMMU_BAR.map_registers(&low, &high, base as u32);
        if iohc::IOMMU_BAR.base_registers(&low, &high) != Some(base) {
            return Err(());
        }
        for old in &mapped {
            let previous = EcamDevice::new(old.root.route.base, 0, 2);
            if previous.read32(iommu::CAPABILITY + 4)
                != (old.registers.base as u32 | iohc::IommuBar::ENABLE::SET.value)
                || previous.read32(iommu::CAPABILITY + 8) != 0
            {
                fstart_log::error!("rs: IOMMU instance alias changed an earlier unit; fail-stop");
                return Err(());
            }
        }
        // SAFETY: this function's BAR readback and the checked DF route
        // identify a full reserved, UC/identity-mapped IOMMU register window.
        let registers = unsafe {
            iommu::capture(
                base,
                device.read32(iommu::CAPABILITY),
                device.read32(iommu::CAPABILITY + 0x0c),
                device.read32(iommu::CAPABILITY + 0x10),
            )
        }
        .ok_or(())?;
        if registers.base != base || registers.translation_enabled() {
            return Err(());
        }
        fstart_log::info!(
            "rs: IOMMU captured IOS={} capability={:#x} control={:#x} EFR={:#x} EFR2={:#x} counters={:#x}; translation off",
            root.ios.physical_index,
            registers.capability,
            registers.control,
            registers.efr,
            registers.efr2,
            registers.counter_config
        );
        mapped.push(Mapped {
            root: *root,
            registers,
        });
    }
    Ok(mapped)
}

/// The IOMMU environment policy (openSIL NbioIommuTbl.h; `Plan` checks the
/// CPU stepping it was validated on), then a fresh register capture.
/// # Safety
/// All mapped units belong to a checked stepping (`rs::Plan`), BSP owns the IOD's SMN,
/// and no DMA consumer has started. Never retry any failure.
pub unsafe fn initialize_policy(mapped: &mut [Mapped], signature: u32) -> Result<(), ()> {
    use fstart_driver_amd::{
        nbio::{
            iohc,
            iommu::{self, rs::Plan},
        },
        smn::Smn,
    };
    use tock_registers::interfaces::Readable;
    if mapped.is_empty() || mapped.len() > 4 {
        return Err(());
    }
    // SAFETY: checked live root, single IOD, BSP-exclusive shared index/data.
    let smn = unsafe { Smn::on_bus(fstart_pci::ecam::GlobalEcam, mapped[0].root.route.base) };
    if !mapped
        .iter()
        .any(|i| i.root.ios.physical_index == 2 && i.root.route.base == 0)
    {
        return Err(());
    }
    let mut plans = heapless::Vec::<Plan, 4>::new();
    for unit in mapped.iter() {
        let plan = Plan::new(
            unit.root.ios.physical_index,
            signature,
            true,
            unit.root.route.base == 0,
        )
        .map_err(|_| ())?;
        plan.preflight(&smn, &unit.registers).map_err(|error| {
            fstart_log::error!(
                "rs: IOMMU SMN instance/policy preflight IOS={} error={:?}",
                unit.root.ios.physical_index,
                error
            );
        })?;
        fstart_log::info!(
            "rs: IOMMU relocated SMN/PCI identity verified IOS={}",
            unit.root.ios.physical_index
        );
        plans.push(plan).map_err(|_| ())?;
    }
    for (plan, unit) in plans.iter().zip(mapped.iter_mut()) {
        // SAFETY: every unit preflighted, translation off and no USB/NVMe/NIC
        // DMA started. B0 policy only, no SNP/AVIC or A0-only workarounds.
        unsafe { plan.install(&smn) }.map_err(|error| {
            fstart_log::error!(
                "rs: IOMMU policy rejected IOS={} error={:?}; no retry",
                unit.root.ios.physical_index,
                error
            );
        })?;
        fstart_log::info!(
            "rs: IOMMU FCH requester forwarding IOS={} SB_LOCATION={:#x}",
            unit.root.ios.physical_index,
            smn.reg(iohc::SB_LOCATION.offset(u32::from(unit.root.ios.physical_index) << 20))
                .get()
        );
        let device = fstart_pci::EcamDevice::new(unit.root.route.base, 0, 2);
        unit.registers = unsafe {
            iommu::capture(
                unit.registers.base,
                device.read32(iommu::CAPABILITY),
                device.read32(iommu::CAPABILITY + 0x0c),
                device.read32(iommu::CAPABILITY + 0x10),
            )
        }
        .ok_or(())?;
        if unit.registers.translation_enabled() {
            return Err(());
        }
        fstart_log::info!(
            "rs: IOMMU L1/L2 policy IOS={} EFR={:#x} EFR2={:#x}; translation off, SNP/AVIC disabled",
            unit.root.ios.physical_index,
            unit.registers.efr,
            unit.registers.efr2
        );
    }
    Ok(())
}
