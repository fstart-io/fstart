//! QEMU q35 RO/A/B immutable RO selector stage (XIP at top of flash).

#![no_std]
#![no_main]

fstart_platform_qemu::entry!(main);

fn main() -> ! {
    fstart_platform_qemu::q35_ab::run_q35_ab_ro(
        &fstart_board_qemu_q35_ab::QEMU_Q35_AB_RO,
        fstart_board_qemu_q35_ab::console(),
    )
}
