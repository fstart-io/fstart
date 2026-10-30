//! The single QEMU stage. QEMU boards have no board work: the platform's
//! flow is the whole `main`.

#![no_std]
#![no_main]

fstart_platform_qemu::entry!(main, sbi_resume: resume);

fn main() -> ! {
    fstart_platform_qemu::virt_riscv64::run(
        &fstart_board_qemu_riscv64::QEMU_RISCV64_VIRT,
        fstart_board_qemu_riscv64::console(),
    )
}

/// OpenSBI returns here before CrabEFI.
fn resume(hart_id: u64, dtb_addr: u64) -> ! {
    fstart_platform_qemu::virt_riscv64::resume_sbi(
        &fstart_board_qemu_riscv64::QEMU_RISCV64_VIRT,
        fstart_board_qemu_riscv64::console(),
        hart_id,
        dtb_addr,
    )
}
