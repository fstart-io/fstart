// SPDX-License-Identifier: MIT OR Apache-2.0
//! ThinkPad T480 postcar: load the ramstage after leaving cache-as-RAM.
#![no_std]
#![no_main]

fstart_platform_intel::postcar_bin!(fstart_board_lenovo_t480::Board);
