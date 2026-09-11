#![no_std]
#![no_main]
fstart_platform_qemu::stage_bin!(
    fstart_platform_qemu::QemuSifiveUProgram<fstart_board_qemu_sifive_u::Board>
);
