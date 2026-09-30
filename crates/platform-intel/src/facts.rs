//! Host-clean board facts and chipset identity shared by build and firmware.
use fstart_core::FlashLayout;

/// A supported hardware binding, selected once in board Rust. Legacy chipset
/// markers carry an independent CPU-package parameter; integrated platforms
/// can own CPU policy directly. Both supply host and runtime associations.
pub trait IntelPlatform: 'static {
    type Config: IntelPlatformConfig + 'static;
    const NAME: &'static str;
    const CAR_BASE: u64;
    const CAR_SIZE: u64;
    const MICROCODE_SIGNATURES: &'static [&'static str];
}

/// Hardware population needed by both host planning and runtime bring-up.
pub trait IntelPlatformConfig {
    fn max_cpus(&self) -> u16;
}

/// Board-owned physical flash and attached-device assets, not build policy.
#[derive(Debug, Clone, Copy)]
pub struct BoardFacts {
    pub flash: FlashLayout,
    /// Physical chip capacity, not inferred from populated partitions.
    pub flash_size: u32,
    /// Board-owned files packaged into the FFS as verified data assets.
    pub data_assets: &'static [&'static str],
}

impl BoardFacts {
    pub const fn new(flash: FlashLayout, flash_size: u32) -> Self {
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
        Self {
            flash,
            flash_size,
            data_assets: &[],
        }
    }

    #[must_use]
    pub const fn with_data_assets(mut self, assets: &'static [&'static str]) -> Self {
        self.data_assets = assets;
        self
    }
}

/// Unconditional original-source binding, consumed by host and firmware alike.
pub trait IntelBoardFacts {
    type Platform: IntelPlatform;
    /// Config lives in `.rodata`, never reconstructed on the CAR stack.
    const CONFIG: &'static <Self::Platform as IntelPlatform>::Config;
    const FACTS: BoardFacts;
}
