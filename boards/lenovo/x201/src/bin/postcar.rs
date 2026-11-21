//! X201 postcar: CAR is gone; load the ramstage from DRAM.

#![no_std]
#![no_main]

fstart_platform_intel::postcar_bin!(fstart_board_lenovo_x201::Board);
