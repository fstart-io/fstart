//! Host-clean board facts. No stage budgets, linker addresses or Cargo relays.
use fstart_core::IntelIfdFlashLayout;

/// Real chipset differences; common stage placement belongs to the family.
#[derive(Debug, Clone, Copy)]
pub enum Chipset {
    Gm965Ich8,
    I945Ich7,
}

#[derive(Debug, Clone, Copy)]
pub struct BoardFacts {
    pub flash: IntelIfdFlashLayout,
    /// Physical chip capacity, not inferred from populated partitions.
    pub flash_size: u32,
    pub max_cpus: u16,
    pub chipset: Chipset,
}
impl BoardFacts {
    pub const fn new(
        flash: IntelIfdFlashLayout,
        flash_size: u32,
        max_cpus: u16,
        chipset: Chipset,
    ) -> Self {
        flash.validate();
        assert!(
            flash_size.is_power_of_two(),
            "flash chip capacity must be a power of two"
        );
        assert!(
            flash.size() == flash_size,
            "IFD map must cover declared chip capacity"
        );
        assert!(max_cpus != 0, "CPU population must be nonzero");
        Self {
            flash,
            flash_size,
            max_cpus,
            chipset,
        }
    }
}

pub trait IntelBoardFacts {
    const FACTS: BoardFacts;
}
