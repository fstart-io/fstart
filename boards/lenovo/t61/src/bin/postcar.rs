//! Load the T61 ramstage after leaving cache-as-RAM.

#![no_std]
#![no_main]

fstart_platform_intel::postcar_bin!(
    fstart_board_lenovo_t61::Board,
    fstart_driver_uart::ns16550::Ns16550,
    fstart_board_lenovo_t61::mainboard::console(),
);
