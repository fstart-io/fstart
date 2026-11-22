//! GA-945GCM postcar: CAR is gone; load the ramstage from DRAM.

#![no_std]
#![no_main]

fstart_platform_intel::postcar_bin!(
    fstart_board_gigabyte_ga_945gcm_s2l::Board,
    fstart_driver_uart::ns16550::Ns16550,
    fstart_board_gigabyte_ga_945gcm_s2l::mainboard::console(),
);
