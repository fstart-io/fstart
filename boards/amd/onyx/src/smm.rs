// SPDX-License-Identifier: MIT OR Apache-2.0
//! The PSP's SMM handler: SmmInfo, its ACPI EINJ service and P2C requests.
fstart_platform_rs::smm::smm_bin!(handler: fstart_platform_rs::rs_smi_handler);
