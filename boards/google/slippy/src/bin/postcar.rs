// SPDX-License-Identifier: MIT OR Apache-2.0
//! Peppy postcar: CAR is gone; load the ramstage from DRAM.

#![no_std]
#![no_main]

fstart_platform_intel::postcar_bin!(fstart_board_google_peppy::Board);
