//! QEMU q35 RO/A/B authenticated RW ramstage (loaded to fixed DRAM by RO).

#![no_std]
#![no_main]

fstart_platform_qemu::entry_ram!(main);

fn main() -> ! {
    fstart_platform_qemu::q35_ab::run_q35_ab_ramstage(
        &fstart_board_qemu_q35_ab::QEMU_Q35_AB_RAMSTAGE,
        fstart_board_qemu_q35_ab::console(),
    )
}
