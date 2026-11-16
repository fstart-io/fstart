// SPDX-License-Identifier: MIT OR Apache-2.0
//! The FCH's SMI handling. The flow hands the OS ACPI directly and
//! installs no handler yet; this is the image the SMM unit builds.

fstart_platform_amd::smm::smm_bin!(handler: fstart_platform_amd::smm::handle);
