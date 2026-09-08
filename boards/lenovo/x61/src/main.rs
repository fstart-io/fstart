#![no_std]
#![no_main]
fstart_platform_intel::stage_bin!(
    fstart_platform_intel::gm965::Program<fstart_board_lenovo_x61::Board>
);
