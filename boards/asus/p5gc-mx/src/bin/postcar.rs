//! P5GC-MX postcar: CAR is gone; load the ramstage from DRAM.

#![no_std]
#![no_main]

fstart_platform_intel::postcar_bin!(
    fstart_board_asus_p5gc_mx::Board,
    fstart_driver_uart::ns16550::Ns16550,
    fstart_board_asus_p5gc_mx::mainboard::console(),
);
