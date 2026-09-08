#![no_std]
#![no_main]
fstart_platform_qemu::stage_bin!(
    fstart_platform_qemu::QemuAarch64Program<fstart_board_qemu_aarch64::Board>
);
