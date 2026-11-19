//! MMIO helpers for Intel GMA display initialization.

use fstart_core::mmio::MmioReadWrite;
use tock_registers::interfaces::{Readable, Writeable};

use fstart_core::typed::{Mmio32, MmioAddr};

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

    /// Clear selected bits in a 32-bit register.
    pub(crate) fn clear_bits32(&self, offset: usize, mask: u32) {
        self.write32(offset, self.read32(offset) & !mask);
    }

    /// Clear and set selected bits in a 32-bit register.
    pub(crate) fn update32(&self, offset: usize, clear: u32, set: u32) {
        self.write32(offset, (self.read32(offset) & !clear) | set);
    }

    /// Poll until `register & mask == value` (libgfxinit `Registers.Wait`).
    ///
    /// Like libgfxinit, the register is read once more after the deadline,
    /// so a late match still counts. Returns whether it matched.
    pub(crate) fn wait_mask(&self, offset: usize, mask: u32, value: u32, timeout_us: u64) -> bool {
        let deadline = now_us().saturating_add(timeout_us);
        let mut timed_out = false;
        loop {
            if self.read32(offset) & mask == value {
                return true;
            }
            if timed_out {
                return false;
            }
            timed_out = now_us() >= deadline;
            core::hint::spin_loop();
        }
    }
}

/// Monotonic microsecond timestamp used for libgfxinit-style deadlines.
pub(crate) fn now_us() -> u64 {
    fstart_arch::x86::timestamp_us()
}

/// Wait until the timestamp `deadline_us` (libgfxinit `Time.Delay_Until`).
pub(crate) fn delay_until(deadline_us: u64) {
    let now = now_us();
    if deadline_us > now {
        delay_us(u32::try_from(deadline_us - now).unwrap_or(u32::MAX));
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
