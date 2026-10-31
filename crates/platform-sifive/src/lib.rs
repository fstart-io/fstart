//! SiFive physical-platform build policy. Board wiring and the fixed FU740
//! hardware flow remain in the board; no physical board is a QEMU machine.
#![no_std]

#[cfg(feature = "host")]
pub mod host;
#[cfg(feature = "bundle-fu740")]
pub use fstart_stage as stage_runtime;
#[cfg(feature = "host")]
pub use host::Plan;
#[macro_export]
macro_rules! stage_bin {
    ($program:ty) => { $crate::stage_runtime::stage_bin!(program: $program, sbi_resume); };
}

/// Board population and payload destinations, shared with its live FU740 flow.
/// The platform owns LIM execution geometry and compiler mechanics.
#[derive(Clone, Copy)]
pub struct Fu740ImageFacts {
    pub flash_base: u64,
    pub flash_size: u64,
    pub dram_base: u64,
    pub dram_size: u64,
    pub kernel_addr: u64,
    pub runtime_addr: u64,
    pub dtb_addr: u64,
    pub dtb_file: &'static str,
    pub bootargs: &'static str,
    pub boot_hart_id: u32,
}
pub trait Fu740BoardFacts {
    const IMAGE: Fu740ImageFacts;
}
