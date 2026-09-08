#![no_std]
#![no_main]
fstart_platform_qemu::stage_bin!(
    fstart_platform_qemu::QemuRiscv64Program<fstart_board_qemu_riscv64::Board>
);
