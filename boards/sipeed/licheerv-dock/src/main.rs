#![no_std]
#![no_main]
fstart_platform_sunxi::stage_bin!(
    fstart_platform_sunxi::d1::Program<fstart_board_licheerv_dock::Board>
);
