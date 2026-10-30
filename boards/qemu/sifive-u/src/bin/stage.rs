//! The single QEMU stage. QEMU boards have no board work: the platform's
//! flow is the whole `main`.

#![no_std]
#![no_main]

fstart_platform_qemu::entry!(main, sbi_resume: resume);

fn main() -> ! {
    fstart_platform_qemu::sifive_u::run(
        &fstart_board_qemu_sifive_u::QEMU_SIFIVE_U,
        &fstart_board_qemu_sifive_u::QEMU_SIFIVE_U_UART,
    )
}

/// OpenSBI returns here before CrabEFI.
fn resume(hart_id: u64, dtb_addr: u64) -> ! {
    fstart_platform_qemu::sifive_u::resume_sbi(
        &fstart_board_qemu_sifive_u::QEMU_SIFIVE_U,
        &fstart_board_qemu_sifive_u::QEMU_SIFIVE_U_UART,
        hart_id,
        dtb_addr,
    )
}
