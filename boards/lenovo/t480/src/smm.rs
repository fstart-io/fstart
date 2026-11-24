// SPDX-License-Identifier: MIT OR Apache-2.0
//! Sunrise Point-LP SMI handler; ThinkPad T480 adds no board-specific SMI work.
fstart_platform_intel::smm::smm_bin!(handler: fstart_driver_intel::sunrise_point::smi::handle);
