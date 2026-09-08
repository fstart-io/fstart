#![no_std]
#![no_main]
fstart_stage::stage_bin!(program: fstart_platform_qemu::QemuArmv7Program<fstart_board_qemu_armv7::Board>);
