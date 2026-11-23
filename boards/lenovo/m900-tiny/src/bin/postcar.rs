// SPDX-License-Identifier: MIT OR Apache-2.0
//! Load the M900 Tiny ramstage after leaving cache-as-RAM.
#![no_std]
#![no_main]

fstart_platform_intel::postcar_bin!(
    fstart_board_lenovo_m900_tiny::Board,
    fstart_driver_uart::ns16550::Ns16550,
    fstart_board_lenovo_m900_tiny::mainboard::console(),
);
