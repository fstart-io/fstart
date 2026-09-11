#![no_std]
#![no_main]
fstart_platform_sunxi::stage_bin!(
    fstart_platform_sunxi::h3::Program<fstart_board_orangepi_pc2::Board>
);
