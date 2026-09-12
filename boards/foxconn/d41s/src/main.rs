#![no_std]
#![no_main]
fstart_platform_intel::stage_bin!(
    fstart_platform_intel::pineview::Program<fstart_board_foxconn_d41s::Board>
);
