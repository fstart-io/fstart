//! The single QEMU stage. QEMU boards have no board work: the platform's
//! flow is the whole `main`.

#![no_std]
#![no_main]

fstart_platform_qemu::entry!(main);

fn main() -> ! {
    fstart_platform_qemu::virt_aarch64::run(
        &fstart_board_qemu_aarch64::QEMU_AARCH64_VIRT,
        fstart_board_qemu_aarch64::console(),
    )
}
