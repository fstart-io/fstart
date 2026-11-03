//! The single QEMU stage. QEMU boards have no board work: the platform's
//! flow is the whole `main`.

#![no_std]
#![no_main]

fstart_platform_qemu::entry!(main);

fn main() -> ! {
    fstart_platform_qemu::run_q35(
        &fstart_board_qemu_q35_i686::QEMU_Q35_PLATFORM,
        fstart_board_qemu_q35_i686::console(),
    )
}
