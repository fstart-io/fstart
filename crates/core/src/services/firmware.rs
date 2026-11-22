//! Firmware image provider service.
//!
//! Hardware that exposes the boot firmware image through memory-mapped
//! apertures implements [`FirmwareImageProvider`].  The provider is the single
//! source of truth for flash-to-CPU address translation: board metadata should not
//! repeat SPI/ROM decode windows that are properties of the chipset or SoC.

use super::ServiceError;

/// One CPU-visible memory-mapped window into a logical firmware image.
///
/// `flash_offset..flash_offset + size` in the logical image is readable at
/// `cpu_base..cpu_base + size`.  A firmware image can have more than one
/// window; modern chipsets may expose different parts of flash through
/// different decode apertures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FirmwareWindow {
    /// Offset within the logical firmware image.
    pub flash_offset: u64,
    /// CPU-visible base address for this window.
    pub cpu_base: u64,
    /// Window size in bytes.
    pub size: u64,
}

impl FirmwareWindow {
    /// Empty window used to fill fixed-size arrays.
    pub const EMPTY: Self = Self {
        flash_offset: 0,
        cpu_base: 0,
        size: 0,
    };

    /// Create a firmware-image window.
    pub const fn new(flash_offset: u64, cpu_base: u64, size: u64) -> Self {
        Self {
            flash_offset,
            cpu_base,
            size,
        }
    }

    /// End offset within the logical firmware image.
    pub const fn flash_end(self) -> Option<u64> {
        self.flash_offset.checked_add(self.size)
    }

    /// End address of the CPU-visible window.
    pub const fn cpu_end(self) -> Option<u64> {
        self.cpu_base.checked_add(self.size)
    }

    /// Return true if `offset` lies inside this window.
    pub fn contains(self, offset: u64) -> bool {
        self.flash_end()
            .is_some_and(|end| offset >= self.flash_offset && offset < end)
    }
}

/// Maximum number of CPU-visible decode windows in one [`FirmwareImage`].
///
/// Six covers AMD paged ROM3 (ROM2 plus four 16 MiB pages on a 64 MiB flash)
/// with one slot to spare.
pub const MAX_FIRMWARE_WINDOWS: usize = 6;

/// Logical firmware image mapping.
///
/// The fixed array keeps this type usable in `no_std` runtime state without
/// allocation while still allowing chipsets with several memory-mapped flash
/// apertures.  Empty slots have `size == 0`; `window_count` records the active
/// prefix.
///
/// Windows may overlap in logical flash space (for example, ROM2 and ROM3 on
/// AMD both expose the booted 16 MiB page). Translation checks active windows
/// in order and returns the **first** match, so preferred apertures (such as
/// ROM2) should appear first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FirmwareImage {
    /// Logical image size in bytes.
    pub size: u64,
    /// Active windows are `windows[..window_count]`.
    pub windows: [FirmwareWindow; MAX_FIRMWARE_WINDOWS],
    /// Number of active entries in [`Self::windows`].
    pub window_count: u8,
}

impl FirmwareImage {
    /// Empty image.
    pub const EMPTY: Self = Self {
        size: 0,
        windows: [FirmwareWindow::EMPTY; MAX_FIRMWARE_WINDOWS],
        window_count: 0,
    };

    /// A single contiguous image window where flash offset 0 maps to `base`.
    pub const fn single_window(base: u64, size: u64) -> Self {
        let mut windows = [FirmwareWindow::EMPTY; MAX_FIRMWARE_WINDOWS];
        windows[0] = FirmwareWindow::new(0, base, size);
        Self {
            size,
            windows,
            window_count: 1,
        }
    }

    /// Reusable legacy x86 SPI decode: `size` bytes immediately below 4 GiB.
    pub const fn x86_top_of_4g(size: u64) -> Self {
        Self::single_window(0x1_0000_0000u64 - size, size)
    }

    /// Return active windows.
    pub fn active_windows(&self) -> &[FirmwareWindow] {
        let count = (self.window_count as usize).min(self.windows.len());
        &self.windows[..count]
    }

    /// Validate image metadata supplied by a provider or platform mapping.
    pub fn validate(&self) -> Result<(), ServiceError> {
        if self.window_count as usize > self.windows.len() {
            return Err(ServiceError::InvalidParam);
        }

        for window in self.active_windows() {
            let flash_end = window.flash_end().ok_or(ServiceError::InvalidParam)?;
            window.cpu_end().ok_or(ServiceError::InvalidParam)?;
            if flash_end > self.size {
                return Err(ServiceError::InvalidParam);
            }
        }

        Ok(())
    }

    /// Return the single contiguous backing window, if the whole image has one.
    pub fn contiguous_window(&self) -> Option<FirmwareWindow> {
        if self.window_count == 1 {
            let window = self.windows[0];
            if window.flash_offset == 0 && window.size == self.size {
                return Some(window);
            }
        }
        None
    }

    /// Return the CPU-visible address for a logical firmware-image offset.
    pub fn translate(&self, offset: u64) -> Option<u64> {
        self.contiguous_chunk(offset, 1).map(|(addr, _)| addr)
    }

    /// Return the CPU-visible address if `offset..offset + len` lies wholly
    /// within one active window (first match wins).
    pub fn translate_range(&self, offset: u64, len: u64) -> Option<u64> {
        let end = offset.checked_add(len)?;
        if end > self.size {
            return None;
        }
        for window in self.active_windows() {
            let Some(win_end) = window.flash_end() else {
                continue;
            };
            if offset >= window.flash_offset && end <= win_end {
                return window.cpu_base.checked_add(offset - window.flash_offset);
            }
        }
        None
    }

    /// Return `(cpu_addr, chunk_len)` for the first active window containing
    /// `offset`, capped to `max_len`.
    pub fn contiguous_chunk(&self, offset: u64, max_len: u64) -> Option<(u64, u64)> {
        if max_len == 0 || offset >= self.size {
            return None;
        }
        for window in self.active_windows() {
            let Some(win_end) = window.flash_end() else {
                continue;
            };
            if offset >= window.flash_offset && offset < win_end {
                let cpu_addr = window.cpu_base.checked_add(offset - window.flash_offset)?;
                let available = win_end - offset;
                return Some((cpu_addr, available.min(max_len)));
            }
        }
        None
    }

    /// Return true if any active CPU-visible window overlaps `start..start + size`.
    pub fn overlaps_cpu_range(&self, start: u64, size: u64) -> bool {
        let Some(end) = start.checked_add(size) else {
            return true;
        };
        if size == 0 {
            return false;
        }
        self.active_windows().iter().any(|window| {
            window.size != 0
                && window
                    .cpu_end()
                    .is_none_or(|win_end| start < win_end && window.cpu_base < end)
        })
    }
}

/// Runtime provider for the boot firmware image mapping.
///
/// Implemented by chipset/SoC drivers whose hardware defines how the firmware
/// image is exposed to the CPU.  Build tooling uses matching driver-side static
/// metadata where available; runtime code calls this trait so stages do not
/// bake board-local copies of hardware decode windows.
pub trait FirmwareImageProvider: Send + Sync {
    /// Return the logical firmware-image mapping currently exposed by hardware.
    fn firmware_image(&self) -> Result<FirmwareImage, ServiceError>;
}
