//! Host-clean hardware facts and explicit payload build policy.
//! No stage budgets, linker addresses or Cargo relays.
use fstart_core::{FlashLayout, board::UefiBuildProfile};

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
    /// Requested payload capabilities, independent of hardware and persistence.
    pub uefi_build_profile: UefiBuildProfile,
    /// Board-directory files packaged into the FFS as verified data assets.
    ///
    /// A name is both the board-directory file and the name firmware looks the
    /// asset up by at runtime, so `VbtSource::ffs(NAME)` and this list cannot
    /// disagree.
    pub data_assets: &'static [&'static str],
    /// Direct x86 Linux (LinuxBoot) policy, when the board boots a kernel
    /// instead of a UEFI firmware.
    ///
    /// One declaration feeds both halves: the host plan packages
    /// `kernel_file` and the ramstage builds the zero page from the same
    /// constants, so the assembled payload and the launch cannot disagree.
    pub linux: Option<X86LinuxBoot>,
}

/// Direct x86 Linux (LinuxBoot) policy.
///
/// x86 has no device tree, so the kernel is entered through the boot protocol:
/// the stage loads the bzImage payload into RAM, builds the zero page from the
/// firmware's own e820 map and ACPI RSDP, and jumps to the kernel's 64-bit
/// entry point.
#[derive(Debug, Clone, Copy)]
pub struct X86LinuxBoot {
    /// Board file packaged as the FFS payload, and the runtime lookup name.
    pub kernel_file: &'static str,
    /// RAM address the payload is loaded to: above every firmware region and
    /// clear of the kernel's own decompression window.
    pub kernel_load_addr: u64,
    /// Zero page (boot_params) address. Free low memory, below the EBDA the
    /// firmware copies the RSDP into.
    pub zero_page_addr: u64,
    /// Kernel command line handed over in the zero page.
    pub bootargs: &'static str,
    /// Print BSP MTRR/control-register state before the jump to Linux.
    pub print_x86_mtrrs: bool,
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
            data_assets: &[],
            linux: None,
        }
    }

    /// Declare board-shipped FFS data assets (see [`Self::data_assets`]).
    #[must_use]
    pub const fn with_data_assets(mut self, assets: &'static [&'static str]) -> Self {
        self.data_assets = assets;
        self
    }

    /// Declare the direct x86 Linux payload this board boots.
    #[must_use]
    pub const fn with_linux_boot(mut self, boot: X86LinuxBoot) -> Self {
        self.linux = Some(boot);
        self
    }

    pub const fn with_uefi_build_profile(mut self, profile: UefiBuildProfile) -> Self {
        self.uefi_build_profile = profile;
        self
    }
}

pub trait IntelBoardFacts {
    const FACTS: BoardFacts;
}
