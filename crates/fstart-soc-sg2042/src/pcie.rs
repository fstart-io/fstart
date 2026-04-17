//! SG2042 Cadence PCIe RC (Root Complex) driver.
//!
//! Covers: sideband init, controller config, Cadence PHY programming,
//! PHY reset sequence, link training, and BAR configuration.
//! Configured as PCIe0, RC mode, x16 Gen4.
//!
//! Hardware reference: `drivers/sophgo/pcie/mango_pcie.c`.
