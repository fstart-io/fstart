// SPDX-License-Identifier: MIT OR Apache-2.0
//! The FCH ACPI-mode, sleep and power-button SMI handling; Vilboz has no
//! SMI sources of its own.

fstart_platform_amd::smm::smm_bin!(handler: fstart_driver_amd::fch::smi::handle);
