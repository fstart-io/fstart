#![no_std]
#![no_main]
fstart_stage::stage_bin!(program: fstart_platform_qemu::QemuAarch64Program<fstart_board_qemu_aarch64::Board>);
