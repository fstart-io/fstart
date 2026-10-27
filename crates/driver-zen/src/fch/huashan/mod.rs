//! Huashan (Rome SSP, Milan GN): the IOD SATA controllers, USB through the
//! SMU, and the SMM parts that differ from Taishan (FCH_SP3 §3, §5, §6).
//!
//! The reset/env/late tasks shared with Taishan are in
//! [`super::common`]; Milan's prelim IOMUX/eMMC writes, its ResetHwAcpi
//! table and the env deltas are selected there by generation.

pub mod sata;
pub mod smm;
pub mod usb;
