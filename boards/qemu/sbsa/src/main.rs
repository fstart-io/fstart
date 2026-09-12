#![no_std]
#![no_main]
fstart_platform_qemu::stage_bin!(
    fstart_platform_qemu::QemuSbsaProgram<fstart_board_qemu_sbsa::Board>
);
