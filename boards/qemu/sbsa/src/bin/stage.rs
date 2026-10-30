//! The single QEMU stage. QEMU boards have no board work: the platform's
//! flow is the whole `main`.

#![no_std]
#![no_main]

fstart_platform_qemu::entry!(main);

fn main() -> ! {
    fstart_platform_qemu::sbsa::run(&fstart_board_qemu_sbsa::QEMU_SBSA)
}
