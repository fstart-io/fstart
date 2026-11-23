// SPDX-License-Identifier: MIT OR Apache-2.0
//! The SoCs of the SP7 socket (WH I/O die + Qilian FCH): currently Venice
//! ([`crate::venice`]).
//!
//! Feature-complete 1P SP7/Venice SoC support (compiled, unit-tested, and
//! verified offline against a 64 MiB MD25-CG0 donor ROM; not yet booted on
//! physical SP7 hardware):
//! - APOB V2 (`9/9` memory map, `9/0x13` NPS, `3/3` dual-IOD `CoreMapV2`,
//!   `8/8` ECC + 32-byte DMI slots, `1/0x11` DDR5 SPD inventory, `7/6` ABL
//!   event log) with dynamic page-rounded APOB reservation.
//! - 1P dual-IOD DFv4.5 bus/MMIO/IO/VGA routing broadcast across all live local
//!   IODs (`0x18`/`0x19`) and DRAM rule decoding (`F7x200` on CCM `0x25`/`0x24`).
//! - WH NBIO: early bus + `SB_LOCATION`, IOHC TOM, 512 KiB IOMMU + L1/L2 init
//!   and IVRS capture, 64 KiB-aligned IOAPIC with 24-bridge INTx routes, AER,
//!   bridge visibility, and PSP BAR (`iohc::map_psp` + C2P responsiveness check).
//! - Dual-IOD WH MPIO v2 (`dxio::wh`, `sp7::mpio`): Gen6 ASK/DELI lane
//!   training, PERST release, and PSP-trained BMC VGA preservation.
//! - Qilian FCH takeover, secondary-IOD FCH init, SMU USB init, GPIO, and
//!   AST2700 Super I/O support.
//! - Zen 6 / Zen 6c (`ccx::wh`): dual-IOD AP release up to 512 logical CPUs,
//!   microcode loading (`0xb500`/`0xb50b`/`0xb510`/`0xbc00`), SMU v14 probe.
//! - Dedicated SP7 SMM (`sp7::smm`) in 16 MiB TSEG with SMCA RAS storm control,
//!   BMC SEL delivery, PSP P2C, and optional `EINJ` (`MP0_C2PMSG_31`).
//! - Full ACPI (`SRAT`, `SLIT`, `MSCT`, `_PXM`, primary-thread-first `MADT`,
//!   `IVRS`, `MCFG`, `HEST`, `BERT`, optional `EINJ`, `WSMT`, active WH `_PRT`)
//!   and SMBIOS Type 16/17/19 from 16-channel DDR5 APOB inventory.
//! - 64 MiB Venice ISH A/B dual-origin flash assembly (`fstart-image-build`).
//! - Multi-socket (2P) SP7 remains future work.

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
    df::{RootBridge, plan, v4},
    fch::{
        gpio::Pad,
        qilian::{self, Perst},
    },
    nbio::wh as nbio_wh,
};
use heapless::Vec;

/// ECAM: 256 buses at `0xE000_0000` on segment 0.
pub const ECAM_BASE: u64 = 0xe000_0000;
pub const ECAM_BUSES: u16 = 256;

/// FCH IOAPIC ID (matches MADT and Qilian FCH defaults).
pub const FCH_IOAPIC_ID: u8 = 0;

/// TSEG at the top of DRAM below 4 GiB: 16 MiB fits up to 512 threads at
/// 8 KiB per SMM stack (`4 MiB`) plus the handler image with headroom.
pub const TSEG_SIZE: u32 = 16 << 20;

/// Base local fabric ID of IOS 0 on a WH IOD (IOS `0..4` = `0x20..0x23`; bit 6
/// selects IOD `0`/`1`, bit 7 selects socket `0`/`1`).
pub const IOS0_LOCAL_FABRIC_ID: u8 = 0x20;
/// Alignment of an NBIO IOAPIC MMIO aperture within `below_pools.non_pci` (64 KiB).
pub const IOAPIC_ALIGN: u64 = 0x1_0000;
/// Size of an NBIO IOAPIC MMIO aperture (4 KiB).
pub const IOAPIC_SIZE: u64 = 0x1000;
/// Size and alignment of a WH IOMMU MMIO BAR (`rb % 4 < 2`, 512 KiB).
pub const IOMMU_BAR_SIZE: u64 = nbio_wh::IOMMU_BAR_SIZE;
/// Size and alignment of the PSP MMIO BAR within `below_pools.non_pci` (1 MiB).
pub const PSP_BAR_SIZE: u64 = 0x10_0000;

/// Per-root-bridge non-PCI MMIO allocations carved strictly inside
/// `rb.below_pools.non_pci`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Bars {
    pub ioapic: Option<u64>,
    pub iommu: Option<u64>,
    pub secondary_fch: Option<u32>,
}

const fn align_up(addr: u64, align: u64) -> Option<u64> {
    let mask = align - 1;
    match addr.checked_add(mask) {
        Some(v) => Some(v & !mask),
        None => None,
    }
}

/// Decode a WH 8-bit DFv4.5 `DramAddressCtl.DstFabricID` into
/// `(socket, cs_on_socket)` for UMC chip-selects (`local_cs < 8` per IOD).
pub const fn decode_wh_dram_dst(dst: u16) -> Option<(u8, u16)> {
    if dst > 0xff {
        return None;
    }
    let raw = dst as u8;
    let socket = (raw >> 7) & 1;
    let die = (raw >> 6) & 1;
    let local_cs = raw & 0x3f;
    if local_cs >= 8 {
        return None;
    }
    Some((socket, (die as u16) * 8 + (local_cs as u16)))
}

/// Normalize a per-IOD or socket-wide physical CCD ID on `die` (`0` or `1`)
/// into the socket-wide physical CCD index (`0..8`) used by `WhGetPhysDomainOfCcd`.
pub const fn socket_phys_ccd(die: u8, phys_ccd: u8) -> u8 {
    if die == 1 && phys_ccd < 4 {
        phys_ccd + 4
    } else {
        phys_ccd
    }
}

/// Decode a WH 8-bit `fabric_id` into `(socket, die, rb_on_iod)`.
pub const fn decode_wh_fabric_id(fabric_id: u16) -> Option<(u8, u8, u8)> {
    if fabric_id > 0xff {
        return None;
    }
    let raw = fabric_id as u8;
    let socket = (raw >> 7) & 1;
    let die = (raw >> 6) & 1;
    let local = raw & 0x3f;
    if local < IOS0_LOCAL_FABRIC_ID || local >= IOS0_LOCAL_FABRIC_ID + nbio_wh::ROOT_BRIDGES_PER_IOD
    {
        return None;
    }
    Some((socket, die, local - IOS0_LOCAL_FABRIC_ID))
}

/// Resolve physical 1P WH root bridges from DF CfgAddressMap `routes` and the
/// list of live local IOD indices (`live_dies`, subset of `[0, 1]`).
///
/// Rejects remote-socket routes, non-zero segments, unknown fabric IDs,
/// duplicate physical root bridges, overlapping bus ranges, and topologies
/// without an explicit bus-0 primary root bridge.
pub fn resolve_wh_roots(
    routes: &[v4::BusRoute],
    live_dies: &[u8],
) -> Result<Vec<RootBridge, 8>, &'static str> {
    if routes.is_empty() || routes.len() > 8 {
        return Err("DFv4.5 root bridge count");
    }
    let mut roots: Vec<RootBridge, 8> = Vec::new();
    for route in routes {
        if route.segment != 0 {
            return Err("DFv4.5 non-zero PCI segment");
        }
        if route.buses.base > route.buses.limit || u16::from(route.buses.limit) >= ECAM_BUSES {
            return Err("DFv4.5 invalid bus range");
        }
        let Some((socket, die, rb_on_iod)) = decode_wh_fabric_id(route.destination) else {
            return Err("DFv4.5 unknown fabric ID");
        };
        if socket != 0 {
            return Err("multi-socket SP7 not yet supported");
        }
        if !live_dies.contains(&die) {
            return Err("DFv4.5 route targets absent IOD");
        }
        let index = die * nbio_wh::ROOT_BRIDGES_PER_IOD + rb_on_iod;
        if roots.iter().any(|r| {
            r.index == index
                || r.fabric_id == route.destination
                || (r.bus_base <= route.buses.limit && route.buses.base <= r.bus_limit)
        }) {
            return Err("DFv4.5 duplicate or overlapping bus route");
        }
        let _ = roots.push(RootBridge {
            socket: 0,
            index,
            fabric_id: route.destination,
            bus_base: route.buses.base,
            bus_limit: route.buses.limit,
            primary: route.buses.base == 0,
        });
    }
    if !roots.iter().any(|rb| rb.primary) {
        return Err("DFv4.5 no bus-0 primary root bridge");
    }
    roots.sort_unstable_by_key(|rb| rb.index);
    Ok(roots)
}

/// Carve non-PCI MMIO apertures strictly inside `non_pci` (`rb.below_pools.non_pci`):
/// - IOMMU (on `rb_index % 4 < 2`): 512 KiB aligned (`IOMMU_BAR_SIZE`) below 4 GiB.
/// - IOAPIC: 64 KiB aligned (`IOAPIC_ALIGN`), 4 KiB window.
/// - PSP BAR (only on `rb_index == 0`, die 0 RB 0): 1 MiB aligned below 4 GiB.
pub fn place_bars(rb_index: u8, non_pci: plan::Window) -> Option<(Bars, Option<u32>)> {
    let end = non_pci.base.checked_add(non_pci.size)?;
    let mut cursor = non_pci.base;

    let mut iommu = None;
    if rb_index % nbio_wh::ROOT_BRIDGES_PER_IOD < nbio_wh::IOMMU_ROOT_BRIDGES_PER_IOD {
        let iommu_base = align_up(cursor, IOMMU_BAR_SIZE)?;
        let iommu_end = iommu_base.checked_add(IOMMU_BAR_SIZE)?;
        if iommu_end > end || iommu_end > (1u64 << 32) {
            return None;
        }
        iommu = Some(iommu_base);
        cursor = iommu_end;
    }

    let ioapic_base = align_up(cursor, IOAPIC_ALIGN)?;
    let ioapic_end = ioapic_base.checked_add(IOAPIC_SIZE)?;
    if ioapic_end > end {
        return None;
    }
    cursor = ioapic_end;

    let mut psp_bar = None;
    if rb_index == 0 {
        let psp_base = align_up(cursor, PSP_BAR_SIZE)?;
        let psp_end = psp_base.checked_add(PSP_BAR_SIZE)?;
        if psp_end > end || psp_end > (1u64 << 32) {
            return None;
        }
        psp_bar = Some(psp_base as u32);
    }

    let mut secondary_fch = None;
    if rb_index == nbio_wh::ROOT_BRIDGES_PER_IOD {
        let fch_base = align_up(cursor, qilian::SECONDARY_ACPI_MMIO_ALIGN)?;
        let fch_end = fch_base.checked_add(qilian::SECONDARY_ACPI_MMIO_SIZE)?;
        if fch_end > end || fch_end > (1u64 << 32) {
            return None;
        }
        secondary_fch = Some(fch_base as u32);
    }

    Some((
        Bars {
            ioapic: Some(ioapic_base),
            iommu,
            secondary_fch,
        },
        psp_bar,
    ))
}

/// One static board PCIe port descriptor for SP7 bring-up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LaneDescriptor {
    /// First MPIO lane on socket 0.
    pub start_lane: u8,
    /// Last MPIO lane (inclusive) on socket 0.
    pub end_lane: u8,
    /// Maximum PCIe generation (`1..=6`).
    pub max_gen: u8,
    /// True if the link is already trained by the PSP/ABL (e.g. BMC VGA).
    pub psp_trained: bool,
}

impl LaneDescriptor {
    /// Number of lanes covered by this descriptor.
    pub const fn width(self) -> u16 {
        if self.end_lane >= self.start_lane {
            (self.end_lane as u16) - (self.start_lane as u16) + 1
        } else {
            0
        }
    }
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
    /// Soldered board devices required to train and enumerate behind their port.
    pub required: Vec<links::Soldered, 32>,
    /// PCIe slot resets released during link bring-up.
    pub perst: &'static [Perst],
    /// Links already trained by the PSP/ABL (first and last lane).
    pub psp_trained: &'static [(u8, u8)],
}

/// A board's configuration of an SP7 SoC.
#[derive(Clone)]
pub struct Config {
    /// Early board GPIO pad table programmed before MPIO.
    pub gpio: &'static [Pad],
    /// WH NBIO / IOMMU policy.
    pub nbio: nbio_wh::Policy,
    /// Zen 6 per-thread MSR / C-state / prefetcher policy.
    pub ccx: ccx::wh::Policy,
    /// MPIO / PERST configuration.
    pub mpio: Mpio,
    /// Qilian xHCI USB cold-boot configuration.
    pub usb: qilian::usb::Policy,
    /// First lane of the board's onboard display root port (`None`: first VGA).
    pub display: Option<u8>,
    /// An LPC/eSPI Super I/O or legacy BMC console is present (FADT
    /// `IAPC_LEGACY_DEVICES`).
    pub legacy_devices: bool,
    /// Populated UMC memory channels (bit per channel; SP7 supports 16 channels).
    pub memory_channels: u16,
    /// Largest DIMM a slot takes, MiB.
    pub max_dimm_mib: u32,
    /// Publish the PSP's ACPI EINJ service when available.
    pub einj: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use fstart_driver_amd::df::BusRange;

    fn overlaps(a_base: u64, a_size: u64, w: plan::Window) -> bool {
        a_size > 0 && w.size > 0 && a_base < w.end() && w.base < a_base + a_size
    }

    #[test]
    fn resolves_dual_iod_wh_routes_by_fabric_id_and_places_bars_outside_pci_pools() {
        // Deliberately unsorted / sparse-order CfgAddressMap entries across 2 IODs.
        let routes = [
            v4::BusRoute {
                segment: 0,
                buses: BusRange {
                    base: 0x80,
                    limit: 0x9f,
                },
                destination: 0x60, // die 1, rb 0 -> index 4
            },
            v4::BusRoute {
                segment: 0,
                buses: BusRange {
                    base: 0x00,
                    limit: 0x1f,
                },
                destination: 0x20, // die 0, rb 0 -> index 0 (primary + PSP)
            },
            v4::BusRoute {
                segment: 0,
                buses: BusRange {
                    base: 0x40,
                    limit: 0x5f,
                },
                destination: 0x22, // die 0, rb 2 -> index 2
            },
            v4::BusRoute {
                segment: 0,
                buses: BusRange {
                    base: 0xc0,
                    limit: 0xdf,
                },
                destination: 0x63, // die 1, rb 3 -> index 7
            },
        ];
        let roots = resolve_wh_roots(&routes, &[0, 1]).unwrap();
        assert_eq!(roots.len(), 4);
        assert_eq!(roots[0].index, 0);
        assert!(roots[0].primary);
        assert_eq!(roots[1].index, 2);
        assert_eq!(roots[2].index, 4);
        assert_eq!(roots[3].index, 7);

        let plan = plan::plan(
            &plan::Input {
                tom: 0x8000_0000,
                tom2: 0x10_0000_0000,
                ecam_base: ECAM_BASE,
                ecam_size: u64::from(ECAM_BUSES) << 20,
                phys_addr_bits: 52,
                root_bridges: &roots,
                mmio_pairs: v4::MMIO_PAIRS,
            },
            &plan::Policy::df3(),
        )
        .unwrap();

        let mut saw_psp = false;
        for rb in &plan.root_bridges {
            let (bars, psp) = place_bars(rb.index, rb.below_pools.non_pci).unwrap();
            let ioapic = bars.ioapic.unwrap();
            assert_eq!(ioapic % IOAPIC_ALIGN, 0);
            assert!(ioapic >= rb.below_pools.non_pci.base);
            assert!(ioapic + IOAPIC_SIZE <= rb.below_pools.non_pci.end());
            assert!(!overlaps(ioapic, IOAPIC_SIZE, rb.below_pools.prefetch));
            assert!(!overlaps(ioapic, IOAPIC_SIZE, rb.below_pools.non_prefetch));
            if rb.index % nbio_wh::ROOT_BRIDGES_PER_IOD < nbio_wh::IOMMU_ROOT_BRIDGES_PER_IOD {
                let iommu = bars.iommu.expect("IOMMU BAR on rb 0..=1");
                assert_eq!(iommu % IOMMU_BAR_SIZE, 0);
                assert!(
                    iommu >= rb.below_pools.non_pci.base
                        && iommu + IOMMU_BAR_SIZE <= rb.below_pools.non_pci.end()
                );
                assert!(ioapic >= iommu + IOMMU_BAR_SIZE);
            } else {
                assert!(bars.iommu.is_none());
            }
            if rb.index == 0 {
                let pbar = u64::from(psp.unwrap());
                assert_eq!(pbar % PSP_BAR_SIZE, 0);
                assert!(pbar >= rb.below_pools.non_pci.base);
                assert!(pbar + PSP_BAR_SIZE <= rb.below_pools.non_pci.end());
                assert!(!overlaps(pbar, PSP_BAR_SIZE, rb.below_pools.prefetch));
                assert!(!overlaps(pbar, PSP_BAR_SIZE, rb.below_pools.non_prefetch));
                assert!(!overlaps(
                    pbar,
                    PSP_BAR_SIZE,
                    plan::Window::new(ioapic, IOAPIC_SIZE)
                ));
                saw_psp = true;
            } else {
                assert!(psp.is_none());
            }
        }
        assert!(saw_psp);
    }

    #[test]
    fn rejects_invalid_wh_routes_and_full_lane_width_does_not_overflow() {
        let base = v4::BusRoute {
            segment: 0,
            buses: BusRange {
                base: 0,
                limit: 0x1f,
            },
            destination: 0x20,
        };
        // Remote socket route rejected.
        assert!(
            resolve_wh_roots(
                &[v4::BusRoute {
                    destination: 0xa0,
                    ..base
                }],
                &[0, 1]
            )
            .is_err()
        );
        // Absent die 1 route rejected.
        assert!(
            resolve_wh_roots(
                &[
                    base,
                    v4::BusRoute {
                        buses: BusRange {
                            base: 0x20,
                            limit: 0x3f
                        },
                        destination: 0x60,
                        segment: 0
                    }
                ],
                &[0]
            )
            .is_err()
        );
        // Overlapping bus range rejected.
        assert!(
            resolve_wh_roots(
                &[
                    base,
                    v4::BusRoute {
                        buses: BusRange {
                            base: 0x10,
                            limit: 0x2f
                        },
                        destination: 0x21,
                        segment: 0
                    }
                ],
                &[0]
            )
            .is_err()
        );
        // Missing bus-0 primary rejected.
        assert!(
            resolve_wh_roots(
                &[v4::BusRoute {
                    buses: BusRange {
                        base: 0x10,
                        limit: 0x1f
                    },
                    ..base
                }],
                &[0]
            )
            .is_err()
        );

        // LaneDescriptor::width handles full 0..=255 without u8 overflow.
        let full = LaneDescriptor {
            start_lane: 0,
            end_lane: 255,
            max_gen: 6,
            psp_trained: false,
        };
        assert_eq!(full.width(), 256);
    }
    #[test]
    fn decode_wh_dram_dst_maps_dual_iod_umc_fabric_ids() {
        assert_eq!(decode_wh_dram_dst(0x00), Some((0, 0)));
        assert_eq!(decode_wh_dram_dst(0x07), Some((0, 7)));
        assert_eq!(decode_wh_dram_dst(0x40), Some((0, 8)));
        assert_eq!(decode_wh_dram_dst(0x47), Some((0, 15)));
        assert_eq!(decode_wh_dram_dst(0x80), Some((1, 0)));
        assert_eq!(decode_wh_dram_dst(0xc7), Some((1, 15)));
        // IOS / CCM fabric IDs (>= 8 local) and >8-bit IDs are rejected.
        assert_eq!(decode_wh_dram_dst(0x20), None);
        assert_eq!(decode_wh_dram_dst(0x100), None);
    }

    #[test]
    fn socket_phys_ccd_normalizes_die1_local_and_global_ids() {
        assert_eq!(socket_phys_ccd(0, 0), 0);
        assert_eq!(socket_phys_ccd(0, 3), 3);
        assert_eq!(socket_phys_ccd(1, 0), 4);
        assert_eq!(socket_phys_ccd(1, 3), 7);
        assert_eq!(socket_phys_ccd(1, 4), 4);
        assert_eq!(socket_phys_ccd(1, 7), 7);
    }
}
