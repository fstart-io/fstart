// SPDX-License-Identifier: MIT OR Apache-2.0
//! The Sunrise Point-H SMI handler; the board adds no SMI work of its own.
fstart_platform_intel::smm::smm_bin!(handler: fstart_driver_intel::sunrise_point::smi::handle);
