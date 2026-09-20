//! Host-clean hardware facts for Intel board families.
//! No payload policy, stage budgets, linker addresses or Cargo relays.
use fstart_core::FlashLayout;

/// Real chipset differences; common stage placement belongs to the family.
#[derive(Debug, Clone, Copy)]
pub enum Chipset {
    Gm965Ich8,
    I945Ich7,
    PineviewIch7,
}

#[derive(Debug, Clone, Copy)]
pub struct BoardFacts {
    pub flash: FlashLayout,
    /// Physical chip capacity, not inferred from populated partitions.
    pub flash_size: u32,
    pub max_cpus: u16,
    pub chipset: Chipset,
    /// Board-owned files packaged into the FFS as verified data assets.
    ///
    /// These describe hardware, such as a board VBT. Payload inputs are selected
    /// by fbuild and do not belong here.
    pub data_assets: &'static [&'static str],
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
            data_assets: &[],
        }
    }

    /// Declare board-owned FFS data assets (see [`Self::data_assets`]).
    #[must_use]
    pub const fn with_data_assets(mut self, assets: &'static [&'static str]) -> Self {
        self.data_assets = assets;
        self
    }
}

pub trait IntelBoardFacts {
    const FACTS: BoardFacts;
}
