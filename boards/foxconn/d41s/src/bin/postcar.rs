//! D41S postcar: CAR is gone; load the ramstage from DRAM.

#![no_std]
#![no_main]

fstart_platform_intel::postcar_bin!(
    fstart_board_foxconn_d41s::Board,
    fstart_driver_uart::ns16550::Ns16550,
    fstart_board_foxconn_d41s::mainboard::console(),
);
