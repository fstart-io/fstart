#![no_std]
#![no_main]
fstart_stage::stage_bin!(program: fstart_platform_qemu::QemuRiscv64Program<fstart_board_qemu_riscv64::Board>);
