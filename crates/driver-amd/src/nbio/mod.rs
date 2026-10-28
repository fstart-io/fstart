// SPDX-License-Identifier: MIT OR Apache-2.0
//! The NBIO (north bridge IO) of a die, all in SMN: the IOHC with its DRAM
//! map and BARs ([`iohc`]), the IOAPIC ([`ioapic`]), the IOMMU ([`iommu`])
//! and the PCIe cores ([`pcie`]), the same IP on Raven/Picasso and Zeppelin.
//! What the SoCs program differently, the vendor register tables and the
//! NBIF setup above all, is [`picasso`] (AGESA AmdNbioBaseRV) and
//! [`zeppelin`] (AmdNbioBaseZP).
pub mod ioapic;
pub mod iohc;
pub mod iommu;
#[cfg(any(feature = "picasso", feature = "zeppelin"))]
pub mod pcie;
#[cfg(feature = "picasso")]
pub mod picasso;
#[cfg(feature = "zeppelin")]
pub mod zeppelin;
