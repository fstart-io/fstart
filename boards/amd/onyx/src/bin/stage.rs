// SPDX-License-Identifier: MIT OR Apache-2.0
//! The reset image: from the PSP's release of the BSP to the payload.
#![no_std]
#![no_main]
fstart_platform_rs::stage_bin!(fstart_board_amd_onyx::Board);
