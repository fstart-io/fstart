//! Rome (SSP) and Milan (GN) NBIO: four quadrants per IOD, shared IP and
//! almost the same tables (NBIO_SMU §3.2–§3.5, §4).
//!
//! PEI order (`AmdNbioBaseInit` SSP `ffdf2242`, GN `ffd1e869`), all
//! sockets:
//! 1. [`sb_location`] on every NBIO;
//! 2. [`socket_table`] on the first NBIO of each socket;
//! 3. per NBIO [`base_init`] (NBIO table, IOAPIC features, bus number, NBIF
//!    bus numbers; Milan NBIF extras and the SDPMUX table), then
//!    [`Nbio::set_smu_bar`] with a 1 MiB non-PCI MMIO block of that NBIO;
//! 4. IOMMU PEI: Milan places the IOMMU BAR ([`Nbio::set_iommu_bar`],
//!    512 KiB below 4 GiB), then per NBIO [`iommu_init`];
//! 5. memory-info PPI: [`Nbio::set_tom`] on every NBIO;
//! 6. PCI enumeration: IOAPIC BAR + ID (and Rome's IOMMU BAR); after it,
//!    [`Nbio::nbif_hw_init_lock`] and [`Nbio::iommu_enable`].

#[rustfmt::skip]
pub mod tables;

use fstart_pci::ConfigRegionAccess;

use super::table::Entry;
use super::{Nbio, NbioPolicy, iohc, iommu};
use crate::sp3::soc::Generation;

struct Tables {
    socket: &'static [Entry],
    nbio: &'static [Entry],
    sdpmux: &'static [Entry],
    iommu: &'static [Entry],
}

/// The table set of `generation`; the SP3 package always selects the
/// socket table (AM4's `SOCKET_AM4` is never taken).
fn tables(generation: Generation) -> Tables {
    match generation {
        #[cfg(feature = "rome")]
        Generation::Rome => Tables {
            socket: tables::rome::SOCKET,
            nbio: tables::rome::NBIO,
            sdpmux: &[],
            iommu: tables::rome::IOMMU,
        },
        #[cfg(feature = "milan")]
        Generation::Milan => Tables {
            socket: tables::milan::SOCKET,
            nbio: tables::milan::NBIO,
            sdpmux: tables::milan::SDPMUX,
            iommu: tables::milan::IOMMU,
        },
        // A handle of a generation this build does not support runs no
        // table (fail closed: callers identify the CPU first).
        _ => Tables {
            socket: &[],
            nbio: &[],
            sdpmux: &[],
            iommu: &[],
        },
    }
}

/// SB location (`ffdf22f5`): the FCH NBIO copies `IOHC::SB_LOCATION` to
/// its IOMMU L1 (IOAGR) and L2; every other NBIO clears it.
pub fn sb_location<P: ConfigRegionAccess + Clone>(nbio: &Nbio<P>) {
    let sb = nbio.at(iohc::SB_LOCATION);
    if nbio.fch {
        if let Some(v) = nbio.read32(sb) {
            nbio.write32(nbio.at(iommu::L1_IOAGR_SP3 + iommu::L1_SB_LOCATION), v);
            nbio.write32(nbio.at(iommu::L2_SB_LOCATION), v);
        }
    } else {
        nbio.write32(sb, 0);
    }
}

/// The socket table (NBIF function exposure, PCIe core 0 bridge hiding on
/// NBIO1–3, SDPMUX/SST/NBIF clock gating, Milan PME_TURNOFF), run through
/// any NBIO of the socket with quadrant 0.
pub fn socket_table<P: ConfigRegionAccess + Clone>(nbio: &Nbio<P>, policy: &NbioPolicy) {
    nbio.run_table(tables(nbio.generation).socket, 0, policy);
}

/// Per-NBIO PEI init after the socket table, without the SMU BAR.
pub fn base_init<P: ConfigRegionAccess + Clone>(nbio: &Nbio<P>, policy: &NbioPolicy) {
    let t = tables(nbio.generation);
    nbio.run_table(t.nbio, nbio.index, policy);
    nbio.ioapic_features();
    nbio.set_bus_number();
    nbio.set_nbif_bus_numbers(policy);
    if nbio.generation == Generation::Milan {
        milan_nbif_extras(nbio, policy);
        nbio.run_table(t.sdpmux, nbio.index, policy);
    }
}

/// Milan `ffd1e0b0` after the bus numbers. The package-class-0x80 branch
/// (hide NBIF0/1 F2) is not SP3 and omitted.
fn milan_nbif_extras<P: ConfigRegionAccess + Clone>(nbio: &Nbio<P>, policy: &NbioPolicy) {
    let field = |v: u8| (v as u32) << 2;
    if let Some(v) = policy.milan.nbif_mgcg {
        (0..3).for_each(|k| {
            nbio.modify32(
                nbio.nbif(k) + super::nbif::MGCG_CTL_LCLK,
                0xFF << 2,
                field(v),
            )
        });
    }
    if let Some(v) = policy.milan.nbif_alt_mgcg {
        [0x0141_0020, 0x0181_0020, 0x0140_3BA8, 0x0180_3BA8]
            .iter()
            .for_each(|&a| nbio.modify32(nbio.at(a), 0xFF << 2, field(v)));
    }
    if policy.milan.pcd_0x70 {
        // Not quadrant-relative in the decompile (INF).
        nbio.modify32(0x0400_0200, 0, 1 << 16);
    }
}

/// IOMMU PEI per NBIO: the IOMMU table, then the EFR/SNP/enable steps.
pub fn iommu_init<P: ConfigRegionAccess + Clone>(nbio: &Nbio<P>, policy: &NbioPolicy) {
    nbio.run_table(tables(nbio.generation).iommu, nbio.index, policy);
    nbio.iommu_post_table(policy);
}

/// SMU-DXE SP3 workaround on every NBIO of the socket: NBIF ALT
/// `0x01811000` bit 11 (`smu.syshub_bit11`).
pub fn smu_syshub_bit11<P: ConfigRegionAccess + Clone>(nbio: &Nbio<P>) {
    nbio.modify32(nbio.at(0x0181_1000), 0, 1 << 11);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sp3::nbio::tests::nbio;

    #[test]
    fn sb_location_fch_and_others() {
        let (m, fch) = nbio(Generation::Rome, 0, 0);
        m.0.borrow_mut()
            .smn
            .insert((0, iohc::SB_LOCATION), 0x0003_0004);
        sb_location(&fch);
        let (m2, other) = nbio(Generation::Rome, 2, 0x40);
        m2.0.borrow_mut().smn.insert((0x40, 0x13D1_007C), 5);
        sb_location(&other);
        assert_eq!(m.0.borrow().smn[&(0, 0x1530_0024)], 0x0003_0004);
        assert_eq!(m.0.borrow().smn[&(0, 0x13F0_112C)], 0x0003_0004);
        assert_eq!(m2.0.borrow().smn[&(0x40, 0x13D1_007C)], 0);
    }

    #[cfg(feature = "rome")]
    #[test]
    fn rome_socket_table_exposure() {
        let (m, n) = nbio(Generation::Rome, 0, 0);
        socket_table(&n, &NbioPolicy::default());
        let s = m.0.borrow();
        let exist = |a: u32| s.smn.get(&(0, a)).copied().unwrap_or(0) & (1 << 28) != 0;
        // SATA on NBIO1/2, CCP on NBIO2, PTDMA everywhere, NTB/HDA off.
        assert!(exist(0x1063_5000) && exist(0x1073_6000));
        assert!(!exist(0x1053_5000) && !exist(0x1083_5000));
        assert!(exist(0x1073_4200) && !exist(0x1053_4200));
        assert!(exist(0x1013_4400) && exist(0x1083_4400));
        assert!(!exist(0x1093_4200) && !exist(0x1073_4800));
    }

    #[cfg(feature = "milan")]
    #[test]
    fn milan_base_init_respects_fuses() {
        use crate::sp3::nbio::Fuses;
        use crate::sp3::smn::model::Model;
        use crate::sp3::topology::RootBridge;
        let model = Model::default();
        let rb = RootBridge {
            socket: 0,
            index: 1,
            fabric_id: 0,
            bus_base: 0x20,
            bus_limit: 0x3f,
            primary: false,
        };
        // NBIF2 of quadrant 1 (aperture 0x10A) fused off.
        let fuses = Fuses {
            word: [0x1000_0000, 0],
        };
        // SAFETY: model.
        let n = unsafe { Nbio::new(model.clone(), Generation::Milan, &rb, fuses) };
        base_init(&n, &NbioPolicy::default());
        let s = model.0.borrow();
        assert!(s.log.iter().all(|&(_, a, _)| a >> 20 != 0x10A));
        assert!(s.smn.contains_key(&(0x20, 0x1063_101C)), "NBIF1 bus strap");
        assert!(
            s.smn.contains_key(&(0x20, 0x04E0_0400)),
            "SDPMUX of quadrant 1"
        );
    }
}
