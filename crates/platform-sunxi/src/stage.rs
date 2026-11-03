//! Blocks every sunxi SoC's two stages share: the SRAM bootblock loads the
//! DRAM stage from the BROM's boot medium, the DRAM stage boots the payload
//! from the same medium.

use fstart_core::description::Range;
use fstart_core::handoff::StageHandoff;
use fstart_core::services::{BlockDevice, ServiceError};
use fstart_driver_uart::ns16550::{Ns16550, Ns16550Config};
use fstart_log::StaticConsole;

/// Bring the UART up and make it the log console.
// Inlined into the board's `main` so its constant console config folds:
// the SRAM budget has no room for a runtime 64-bit baud divisor.
#[inline(always)]
pub(crate) fn console(
    slot: &'static StaticConsole<Ns16550>,
    config: Ns16550Config,
) -> Result<&'static Ns16550, ServiceError> {
    let mut console = Ns16550::new(config).map_err(|_| ServiceError::HardwareError)?;
    console.init().map_err(|_| ServiceError::HardwareError)?;
    Ok(slot.install(console))
}

/// Load the DRAM stage from `media` into trained `dram`, leave the handoff
/// at `handoff_addr` and jump to it.
#[inline(always)]
pub(crate) fn enter_dram_stage(
    media: &impl BlockDevice,
    media_base: u64,
    dram: Range,
    load_addr: u64,
    handoff_addr: u64,
) -> Result<core::convert::Infallible, ServiceError> {
    let entry = crate::boot::load_mainstage(media, media_base, dram, load_addr, handoff_addr)?;
    // SAFETY: `load_mainstage` checked the handoff buffer lies in the
    // trained DRAM and kept it out of the DRAM stage's load window.
    unsafe { fstart_stage::next_stage::write_handoff(dram.size, handoff_addr) }
        .map_err(|_| ServiceError::HardwareError)?;
    #[cfg(target_arch = "arm")]
    fstart_arch::armv7::jump_to_with_handoff(entry, handoff_addr as usize);
    #[cfg(target_arch = "aarch64")]
    fstart_arch::aarch64::jump_to_with_handoff(entry, handoff_addr as usize);
    #[cfg(target_arch = "riscv64")]
    fstart_arch::riscv64::jump_to_with_handoff(entry, handoff_addr as usize)
}

/// The bootblock's handoff at `addr`, which must be where the board put it.
pub(crate) fn handoff(addr: usize, expected: u64) -> StageHandoff {
    if addr != expected as usize {
        fstart_arch::halt();
    }
    // SAFETY: the fixed predecessor reserves this configured buffer and
    // validates it against trained DRAM before jumping here.
    match unsafe { fstart_stage::handoff::try_deserialize(addr) } {
        Some(handoff) => handoff,
        None => fstart_arch::halt(),
    }
}

/// The boot medium the BROM booted from.
#[cfg(any(feature = "a20", feature = "h3"))]
pub enum BootMedia<M, S> {
    Mmc(M),
    Spi(S),
}

#[cfg(any(feature = "a20", feature = "h3"))]
impl<M: BlockDevice, S: BlockDevice> BlockDevice for BootMedia<M, S> {
    fn read(&self, offset: u64, buf: &mut [u8]) -> Result<usize, ServiceError> {
        match self {
            Self::Mmc(mmc) => mmc.read(offset, buf),
            Self::Spi(flash) => flash.read(offset, buf),
        }
    }

    fn write(&self, offset: u64, buf: &[u8]) -> Result<usize, ServiceError> {
        match self {
            Self::Mmc(mmc) => mmc.write(offset, buf),
            Self::Spi(flash) => flash.write(offset, buf),
        }
    }

    fn size(&self) -> u64 {
        match self {
            Self::Mmc(mmc) => mmc.size(),
            Self::Spi(flash) => flash.size(),
        }
    }

    fn block_size(&self) -> u32 {
        match self {
            Self::Mmc(mmc) => mmc.block_size(),
            Self::Spi(flash) => flash.block_size(),
        }
    }
}

/// The DRAM stage's devices, handed to the payload launcher: the FFS is
/// read from the boot medium, not from a mapped window.
#[cfg(any(feature = "a20", feature = "h3"))]
pub struct DramStage<M> {
    console: &'static Ns16550,
    #[cfg_attr(not(feature = "linux"), allow(dead_code))]
    media: M,
    /// Image offset on the boot medium (8 KiB eGON MMC, 0 SPI).
    #[cfg_attr(not(feature = "linux"), allow(dead_code))]
    media_base: u64,
    dram: Range,
}

#[cfg(any(feature = "a20", feature = "h3"))]
impl<M: BlockDevice> DramStage<M> {
    /// Re-open the boot medium and mount the image the bootblock booted.
    pub(crate) fn new(
        soc: &str,
        console: &'static Ns16550,
        media: M,
        media_base: u64,
        handoff: &StageHandoff,
        dram_base: u64,
    ) -> Self {
        if crate::boot::install_mainstage_locator(&media, media_base, handoff).is_err() {
            fstart_arch::halt();
        }
        let dram = Range::new(dram_base, handoff.dram_size.get());
        fstart_log::info!("{} mainstage: {} MiB DRAM", soc, dram.size >> 20);
        Self {
            console,
            media,
            media_base,
            dram,
        }
    }

    /// Boot the build's payload: Linux gets `bootargs` in a device tree
    /// prepared at `fdt_addr`.
    pub(crate) fn boot(self, handoff_addr: u64, bootargs: &'static str, fdt_addr: u64) -> ! {
        #[cfg(fstart_payload = "linux")]
        if crate::boot::install_mainstage_policy(
            self.dram.base,
            self.dram.size,
            handoff_addr,
            fdt_addr,
        )
        .is_err()
        {
            fstart_log::error!("sunxi: invalid mainstage memory policy");
            fstart_arch::halt();
        }
        #[cfg(not(fstart_payload = "linux"))]
        let _ = (handoff_addr, bootargs, fdt_addr);
        let memory = [fstart_core::services::memory_detect::E820Entry::new(
            self.dram.base,
            self.dram.size,
            fstart_core::services::memory_detect::E820Kind::Ram,
        )];
        let mut description = fstart_payload::Description::new(&memory, Default::default());
        description.console = Some(self.console);
        #[allow(unused_mut)]
        let mut placement = fstart_payload::Placement::default();
        #[cfg(fstart_payload = "linux")]
        {
            placement.device_tree = fdt_addr;
            placement.bootargs = bootargs;
        }
        <Selected as fstart_payload::MainstagePayload<Self>>::boot(
            fstart_payload::Handoff::new(description, placement),
            self,
        )
    }
}

#[cfg(all(fstart_payload = "linux", any(feature = "a20", feature = "h3")))]
type Selected = LinuxPayload;
#[cfg(all(not(fstart_payload = "linux"), any(feature = "a20", feature = "h3")))]
type Selected = fstart_payload::HaltPayload;

/// Linux from the boot medium: verify the FFS, load the kernel (and BL31 on
/// AArch64) and the device tree, then jump.
#[cfg(all(feature = "linux", any(feature = "a20", feature = "h3")))]
pub struct LinuxPayload;

#[cfg(all(feature = "linux", any(feature = "a20", feature = "h3")))]
impl<M: BlockDevice> fstart_payload::MainstagePayload<DramStage<M>> for LinuxPayload {
    fn boot(handoff: fstart_payload::Handoff<'_>, stage: DramStage<M>) -> ! {
        use fstart_stage::step;
        let placement = handoff.placement;
        let media = &stage.media;
        let mut boot = fstart_stage::fixed_helpers::BlockDeviceLinuxBoot::new(stage.media_base, 0);
        step("FFS mount", || {
            boot.mount(
                media,
                fstart_stage::anchor::image_size(),
                fstart_stage::fstart_anchor_bytes(),
            )
        });
        step("FFS verify", || boot.verify(media));
        #[cfg(target_arch = "aarch64")]
        step("BL31 load", || boot.load_firmware(media));
        step("kernel load", || boot.load_kernel(media));
        step("FDT load", || boot.load_fdt(media));
        if placement.device_tree != 0 {
            step("FDT prepare", || {
                boot.prepare_fdt(
                    placement.device_tree,
                    placement.bootargs,
                    stage.dram.base,
                    stage.dram.size,
                )
            });
        }
        let params = boot.boot_params(placement.bootargs);
        #[cfg(target_arch = "arm")]
        fstart_arch::armv7::boot_linux(&params);
        #[cfg(target_arch = "aarch64")]
        fstart_arch::aarch64::boot_linux(&params)
    }
}
