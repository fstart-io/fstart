//! MMIO helpers for Intel GMA display initialization.

use fstart_core::mmio::MmioReadWrite;
use tock_registers::interfaces::{Readable, Writeable};

use fstart_core::typed::{Mmio32, MmioAddr};

use crate::error::GmaError;

/// Volatile 32-bit MMIO register window.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Mmio {
    base: *mut u8,
}

impl Mmio {
    /// Create a register window from a physical MMIO base.
    ///
    /// # Safety
    ///
    /// The caller must ensure `base` is mapped and points at the GMA MMIO BAR
    /// for the duration of all accesses through the returned value.
    pub(crate) const unsafe fn new(base: MmioAddr<Mmio32>) -> Self {
        Self {
            base: base.raw() as *mut u8,
        }
    }

    /// Read a 32-bit register at byte `offset`.
    pub(crate) fn read32(&self, offset: usize) -> u32 {
        self.reg32(offset).get()
    }

    /// Write a 32-bit register at byte `offset`.
    pub(crate) fn write32(&self, offset: usize, value: u32) {
        self.reg32(offset).set(value);
    }

    fn reg32(&self, offset: usize) -> &'static MmioReadWrite<u32> {
        // SAFETY: construction requires a valid MMIO window; offsets are chosen
        // from generation register tables and point to aligned 32-bit GMA
        // registers. The returned reference is only used for volatile register
        // access through tock-registers.
        unsafe { self.reg_block(offset) }
    }

    /// Return a typed register block at byte `offset` from this MMIO window.
    ///
    /// # Safety
    ///
    /// The caller must ensure `offset` points at a valid instance of `T` within
    /// the decoded GMA MMIO BAR and that the block's register layout matches
    /// the hardware generation being accessed.
    pub(crate) unsafe fn reg_block<T>(&self, offset: usize) -> &'static T {
        unsafe { &*(self.base.add(offset) as *const T) }
    }

    /// Read back a register to flush posted writes.
    pub(crate) fn posting_read(&self, offset: usize) {
        let _ = self.read32(offset);
    }

    /// Set selected bits in a 32-bit register.
    pub(crate) fn set_bits32(&self, offset: usize, mask: u32) {
        self.write32(offset, self.read32(offset) | mask);
    }

    /// Clear selected bits in a 32-bit register.
    pub(crate) fn clear_bits32(&self, offset: usize, mask: u32) {
        self.write32(offset, self.read32(offset) & !mask);
    }

    /// Clear and set selected bits in a 32-bit register.
    pub(crate) fn update32(&self, offset: usize, clear: u32, set: u32) {
        self.write32(offset, (self.read32(offset) & !clear) | set);
    }

    /// Wait until all bits in `mask` are set.
    #[allow(dead_code)]
    pub(crate) fn wait_set32(
        &self,
        offset: usize,
        mask: u32,
        mut timeout: u32,
    ) -> Result<(), GmaError> {
        while timeout != 0 {
            if (self.read32(offset) & mask) == mask {
                return Ok(());
            }
            timeout -= 1;
            core::hint::spin_loop();
        }
        Err(GmaError::Timeout)
    }

    /// Wait until all bits in `mask` are clear.
    #[allow(dead_code)]
    pub(crate) fn wait_clear32(
        &self,
        offset: usize,
        mask: u32,
        mut timeout: u32,
    ) -> Result<(), GmaError> {
        while timeout != 0 {
            if (self.read32(offset) & mask) == 0 {
                return Ok(());
            }
            timeout -= 1;
            core::hint::spin_loop();
        }
        Err(GmaError::Timeout)
    }
}

/// Short busy delay used around legacy DPLL enable/disable sequencing.
///
/// Delegates to the architecture delay (`TSC`-based on x86_64 during normal
/// stages, POST-port based under SMM) so GMA timing matches the rest of the
/// Intel drivers instead of using an uncalibrated spin loop.
pub(crate) fn delay_us(us: u32) {
    fstart_arch::udelay(us);
}
