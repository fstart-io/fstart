#![no_std]
#![no_main]
fstart_platform_qemu::stage_bin!(
    fstart_platform_qemu::QemuQ35Program<fstart_board_qemu_q35::Board>
);
