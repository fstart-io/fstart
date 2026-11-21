//! D945GCLF postcar: CAR is gone; load the ramstage from DRAM.

#![no_std]
#![no_main]

fstart_platform_intel::postcar_bin!(
    fstart_board_intel_d945gclf::Board,
    fstart_driver_uart::ns16550::Ns16550,
    fstart_board_intel_d945gclf::mainboard::console(),
);
