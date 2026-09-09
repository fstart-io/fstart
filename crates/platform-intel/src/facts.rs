//! Host-clean hardware facts and explicit payload build policy.
//! No stage budgets, linker addresses or Cargo relays.
use fstart_core::{FlashLayout, board::UefiBuildProfile};

/// Real chipset differences; common stage placement belongs to the family.
#[derive(Debug, Clone, Copy)]
pub enum Chipset {
    Gm965Ich8,
    I945Ich7,
}

#[derive(Debug, Clone, Copy)]
pub struct BoardFacts {
    pub flash: FlashLayout,
    /// Physical chip capacity, not inferred from populated partitions.
    pub flash_size: u32,
    pub max_cpus: u16,
    pub chipset: Chipset,
    /// Requested payload capabilities, independent of hardware and persistence.
    pub uefi_build_profile: UefiBuildProfile,
}
impl BoardFacts {
    pub const fn new(flash: FlashLayout, flash_size: u32, max_cpus: u16, chipset: Chipset) -> Self {
        flash.validate();
        assert!(
            flash_size.is_power_of_two(),
            "flash chip capacity must be a power of two"
        );
        let layout_size = match flash {
            FlashLayout::IntelIfd(ifd) => ifd.size(),
            FlashLayout::X86Legacy(legacy) => legacy.size,
        };
        assert!(
            layout_size == flash_size,
            "layout must cover declared chip capacity"
        );
        assert!(max_cpus != 0, "CPU population must be nonzero");
        Self {
            flash,
            flash_size,
            max_cpus,
            chipset,
            uefi_build_profile: UefiBuildProfile::Full,
        }
    }

    pub const fn with_uefi_build_profile(mut self, profile: UefiBuildProfile) -> Self {
        self.uefi_build_profile = profile;
        self
    }
}

pub trait IntelBoardFacts {
    const FACTS: BoardFacts;
}
