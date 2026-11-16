// SPDX-License-Identifier: MIT OR Apache-2.0
//! SMM for the PSP: its SMM info, its P2C requests and its EINJ service
//! (`fstart_platform_amd::rs::smm`).

fstart_platform_amd::smm::smm_bin!(handler: fstart_platform_amd::rs::smm::handle);
