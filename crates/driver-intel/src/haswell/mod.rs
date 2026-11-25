//! Intel Haswell System Agent northbridge, registers, and native DDR3/DDR3L raminit.

#[cfg(not(target_arch = "x86"))]
use fstart_core::mmio::BarReg;
use fstart_core::mmio::MmioWindow;
#[cfg(not(target_arch = "x86"))]
use tock_registers::interfaces::{Readable, Writeable};

pub mod acpi;
pub mod igd;
mod links;
pub mod peg;
pub mod raminit;
pub mod registers;
pub mod system_agent;

pub use igd::{HaswellIgdConfig, Panel};
pub use peg::Peg;
pub use system_agent::{ECAM, Haswell, HaswellConfig, HaswellPch, PchResources, Slots};

/// Size of the Haswell MCHBAR aperture (32 KiB).
pub const MCHBAR_SIZE: usize = 0x8000;

/// Size of the Haswell DMIBAR aperture (4 KiB).
pub const DMIBAR_SIZE: usize = 0x1000;

/// Size of the Haswell EPBAR aperture (4 KiB).
pub const EPBAR_SIZE: usize = 0x1000;

/// An enabled, identity-mapped Haswell MCHBAR window (32 KiB).
#[derive(Clone, Copy)]
pub struct MchBar(MmioWindow);

impl MchBar {
    /// Construct an MCHBAR accessor from its 32 KiB-aligned base address.
    ///
    /// # Safety
    /// `base` must be the mapped, enabled 32 KiB MCHBAR for this CPU. The
    /// caller owns the controller throughout pre-memory initialization.
    pub unsafe fn new(base: usize) -> Self {
        assert!(base != 0 && base.is_multiple_of(MCHBAR_SIZE));
        // SAFETY: caller guarantees `[base, base + MCHBAR_SIZE)` is mapped.
        Self(unsafe { MmioWindow::new(base, MCHBAR_SIZE as u32) })
    }

    /// Wrap an already-validated 32 KiB [`MmioWindow`] (for example a
    /// RAM-backed aperture in host unit tests).
    ///
    /// # Panics
    /// Panics if `window.size() as usize != MCHBAR_SIZE`.
    #[must_use]
    pub const fn from_window(window: MmioWindow) -> Self {
        assert!(window.size() as usize == MCHBAR_SIZE);
        Self(window)
    }

    /// Return the underlying bounded [`MmioWindow`].
    #[must_use]
    pub const fn window(self) -> MmioWindow {
        self.0
    }

    #[inline]
    #[must_use]
    pub fn read8(self, off: u32) -> u8 {
        self.0.read8(off)
    }

    #[inline]
    pub fn write8(self, off: u32, val: u8) {
        self.0.write8(off, val);
    }

    #[inline]
    #[must_use]
    pub fn read16(self, off: u32) -> u16 {
        self.0.read16(off)
    }

    #[inline]
    pub fn write16(self, off: u32, val: u16) {
        self.0.write16(off, val);
    }

    #[inline]
    #[must_use]
    pub fn read32(self, off: u32) -> u32 {
        self.0.read32(off)
    }

    #[inline]
    pub fn write32(self, off: u32, val: u32) {
        self.0.write32(off, val);
    }

    /// Single-transaction 64-bit MMIO read (8-byte aligned).
    ///
    /// On 32-bit x86, uses `movq` via `mm0` (preserving `mm0` and issuing
    /// `emms`) so hardware registers that advance per bus transaction see a
    /// single qword access rather than two split dwords.
    #[inline]
    #[must_use]
    pub fn read64(self, off: u32) -> u64 {
        #[cfg(not(target_arch = "x86"))]
        {
            self.0.reg(BarReg::<u64>::new(off)).get()
        }
        #[cfg(target_arch = "x86")]
        {
            let addr = self.0.register_address(off, 8);
            assert!(addr.is_multiple_of(8));
            // SAFETY: bounds and 8-byte alignment checked; window mapping
            // guaranteed by `MmioWindow::new`. `mm0` is saved and restored and
            // `emms` clears the MMX tag state. As in coreboot no x87 data is live.
            unsafe {
                let mut save = core::mem::MaybeUninit::<u64>::uninit();
                let mut result = core::mem::MaybeUninit::<u64>::uninit();
                core::arch::asm!(
                    "movq [{save}], mm0",
                    "movq mm0, [{src}]",
                    "movq [{dst}], mm0",
                    "movq mm0, [{save}]",
                    "emms",
                    "mfence",
                    save = in(reg) save.as_mut_ptr(),
                    src = in(reg) addr,
                    dst = in(reg) result.as_mut_ptr(),
                    options(nostack, preserves_flags),
                );
                result.assume_init()
            }
        }
    }

    /// Single-transaction 64-bit MMIO write (8-byte aligned).
    ///
    /// On 32-bit x86, uses `movq` via `mm0` (preserving `mm0` and issuing
    /// `emms`) so CADB and REUT registers that advance on each bus transaction
    /// see one qword store.
    #[inline]
    pub fn write64(self, off: u32, val: u64) {
        #[cfg(not(target_arch = "x86"))]
        {
            self.0.reg(BarReg::<u64>::new(off)).set(val);
        }
        #[cfg(target_arch = "x86")]
        {
            let addr = self.0.register_address(off, 8);
            assert!(addr.is_multiple_of(8));
            // SAFETY: bounds and 8-byte alignment checked; window mapping
            // guaranteed by `MmioWindow::new`. `mm0` is saved and restored and
            // `emms` clears the MMX tag state. As in coreboot no x87 data is live.
            unsafe {
                let mut save = core::mem::MaybeUninit::<u64>::uninit();
                core::arch::asm!(
                    "mfence",
                    "movq [{save}], mm0",
                    "movq mm0, [{src}]",
                    "movq [{dst}], mm0",
                    "movq mm0, [{save}]",
                    "emms",
                    "mfence",
                    save = in(reg) save.as_mut_ptr(),
                    src = in(reg) &val,
                    dst = in(reg) addr,
                    options(nostack, preserves_flags),
                );
            }
        }
    }

    #[inline]
    pub fn setbits8(self, off: u32, bits: u8) {
        self.0.setbits8(off, bits);
    }

    #[inline]
    pub fn clrsetbits8(self, off: u32, clear: u8, set: u8) {
        self.0.clrsetbits8(off, clear, set);
    }

    #[inline]
    pub fn setbits32(self, off: u32, bits: u32) {
        self.0.setbits32(off, bits);
    }

    #[inline]
    pub fn clrbits32(self, off: u32, bits: u32) {
        self.0.clrbits32(off, bits);
    }

    #[inline]
    pub fn clrsetbits32(self, off: u32, clear: u32, set: u32) {
        self.0.clrsetbits32(off, clear, set);
    }

    /// Run a PCODE mailbox command: `BIOS_MAILBOX_INTERFACE` (`0x5da4`, 32 bits,
    /// bit 31 `RUN_BUSY`) with `BIOS_MAILBOX_DATA` (`0x5da0`, 32 bits). Returns
    /// the data register after completion.
    pub fn pcode(
        self,
        command: u32,
        data: Option<u32>,
    ) -> Result<u32, fstart_core::services::ServiceError> {
        let idle = || {
            fstart_arch::x86::wait_us(1000, || self.read32(0x5da4) >> 31 == 0)
                .then_some(())
                .ok_or(fstart_core::services::ServiceError::Timeout)
        };
        idle()?;
        if let Some(value) = data {
            self.write32(0x5da0, value);
        }
        self.write32(0x5da4, (1 << 31) | command);
        idle()?;
        let status = self.read32(0x5da4) & 0xff;
        if status != 0 {
            fstart_log::warn!(
                "PCODE {:#x}: mailbox completion error {:#x}",
                command,
                status
            );
            return Err(fstart_core::services::ServiceError::HardwareError);
        }
        Ok(self.read32(0x5da0))
    }
}

/// An enabled, identity-mapped Haswell DMIBAR window (4 KiB).
#[derive(Clone, Copy)]
pub struct DmiBar(MmioWindow);

impl DmiBar {
    /// Construct a DMIBAR accessor from its 4 KiB-aligned base address.
    ///
    /// # Safety
    /// `base` must be the mapped, enabled 4 KiB DMIBAR for this CPU.
    pub unsafe fn new(base: usize) -> Self {
        assert!(base != 0 && base.is_multiple_of(DMIBAR_SIZE));
        // SAFETY: caller guarantees `[base, base + DMIBAR_SIZE)` is mapped.
        Self(unsafe { MmioWindow::new(base, DMIBAR_SIZE as u32) })
    }

    /// Wrap an already-validated 4 KiB [`MmioWindow`].
    #[must_use]
    pub const fn from_window(window: MmioWindow) -> Self {
        assert!(window.size() as usize == DMIBAR_SIZE);
        Self(window)
    }

    #[inline]
    #[must_use]
    pub fn read16(self, off: u32) -> u16 {
        self.0.read16(off)
    }

    #[inline]
    pub fn write32(self, off: u32, val: u32) {
        self.0.write32(off, val);
    }

    #[inline]
    pub fn clrsetbits8(self, off: u32, clear: u8, set: u8) {
        self.0.clrsetbits8(off, clear, set);
    }

    #[inline]
    pub fn setbits16(self, off: u32, bits: u16) {
        self.0.setbits16(off, bits);
    }

    #[inline]
    pub fn setbits32(self, off: u32, bits: u32) {
        self.0.setbits32(off, bits);
    }

    #[inline]
    pub fn clrbits32(self, off: u32, bits: u32) {
        self.0.clrbits32(off, bits);
    }

    #[inline]
    pub fn clrsetbits32(self, off: u32, clear: u32, set: u32) {
        self.0.clrsetbits32(off, clear, set);
    }
}

/// An enabled, identity-mapped Haswell EPBAR window (4 KiB).
#[derive(Clone, Copy)]
pub struct EpBar(MmioWindow);

impl EpBar {
    /// Construct an EPBAR accessor from its 4 KiB-aligned base address.
    ///
    /// # Safety
    /// `base` must be the mapped, enabled 4 KiB EPBAR for this CPU.
    pub unsafe fn new(base: usize) -> Self {
        assert!(base != 0 && base.is_multiple_of(EPBAR_SIZE));
        // SAFETY: caller guarantees `[base, base + EPBAR_SIZE)` is mapped.
        Self(unsafe { MmioWindow::new(base, EPBAR_SIZE as u32) })
    }

    /// Wrap an already-validated 4 KiB [`MmioWindow`].
    #[must_use]
    pub const fn from_window(window: MmioWindow) -> Self {
        assert!(window.size() as usize == EPBAR_SIZE);
        Self(window)
    }

    #[inline]
    #[must_use]
    pub fn read16(self, off: u32) -> u16 {
        self.0.read16(off)
    }

    #[inline]
    pub fn write32(self, off: u32, val: u32) {
        self.0.write32(off, val);
    }

    #[inline]
    pub fn clrsetbits32(self, off: u32, clear: u32, set: u32) {
        self.0.clrsetbits32(off, clear, set);
    }
}
