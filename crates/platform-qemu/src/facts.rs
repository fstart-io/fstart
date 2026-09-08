//! Host-clean image facts for the migrated AArch64 virt board contract.
//! Runtime geometry remains the validated linked descriptor, not these facts.

#[derive(Debug, Clone, Copy)]
pub struct Aarch64ImageFacts {
    pub flash_capacity: u64,
}
impl Aarch64ImageFacts {
    pub const fn new(flash_capacity: u64) -> Self {
        assert!(
            flash_capacity == 0x0800_0000,
            "AArch64 virt requires its two fixed 64-MiB flash banks"
        );
        Self { flash_capacity }
    }
}
pub trait Aarch64BoardFacts {
    const IMAGE: Aarch64ImageFacts;
}
