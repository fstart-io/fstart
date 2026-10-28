// SPDX-License-Identifier: MIT OR Apache-2.0
//! The FCH ACPI-mode, sleep and power-button SMI dispatcher.
use fstart_driver_amd::fch::smi::{FchSmmHandler, FchSmmHooks};

pub struct VilbozSmm;
impl FchSmmHooks for VilbozSmm {}

fstart_platform_amd::smm::smm_bin!(crate::Board, FchSmmHandler<VilbozSmm>);
