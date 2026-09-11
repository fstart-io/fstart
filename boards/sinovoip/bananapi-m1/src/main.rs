#![no_std]
#![no_main]
fstart_platform_sunxi::stage_bin!(
    fstart_platform_sunxi::a20::Program<fstart_board_bananapi_m1::Board>
);
