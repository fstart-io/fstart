// SPDX-License-Identifier: MIT OR Apache-2.0
//! Exclusive temporary endpoint BAR ownership, shared by native SATA/xDCI.
//! No sizing probe, resource allocator, ECAM transport or hardware rollback.
use fstart_pci::{EcamDevice, PCI_COMMAND_BITS};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Error {
    Invalid,
    Owned,
    Readback,
}
struct Saved {
    device: EcamDevice,
    offset: u16,
    low: u32,
    high: Option<u32>,
    command: u16,
}
pub(crate) struct TemporaryBar<T> {
    saved: Option<Saved>,
    base: usize,
    _view: core::marker::PhantomData<(T, *mut ())>,
}
impl<T> TemporaryBar<T> {
    /// # Safety
    /// Exclusive ownership of the actual quiescent type-0 endpoint and this
    /// whole real BAR. `size` is its known decode aperture, NOT a fabricated
    /// sizing result. The complete reserved non-DRAM window is mapped UC and
    /// valid for T's MMIO register layout throughout this lease. No AP, SMI,
    /// DMA or other agent may use/reassign the BAR/command/window. Temporary
    /// decode is not permission to start DMA or leave an active engine behind.
    pub(crate) unsafe fn bind(
        device: EcamDevice,
        offset: u16,
        base: usize,
        size: usize,
    ) -> Result<Self, Error> {
        if !(0x10..=0x24).contains(&offset)
            || offset & 3 != 0
            || device.read8(0xe) & 0x7f != 0
            || base == 0
            || !size.is_power_of_two()
            || base & (size - 1) != 0
            || size < size_of::<T>()
            || !base.is_multiple_of(align_of::<T>())
            || base.checked_add(size).is_none()
        {
            return Err(Error::Invalid);
        }
        let low = device.read32(offset);
        let wide = match low & 7 {
            0 => false,
            4 if offset < 0x24 => true,
            _ => return Err(Error::Invalid),
        };
        if !wide && base as u64 + size as u64 > 0x1_0000_0000 {
            return Err(Error::Invalid);
        }
        let high = wide.then(|| device.read32(offset + 4));
        let command = device.read16(4);
        if low & !15 != 0 || high.is_some_and(|value| value != 0) || command & 7 != 0 {
            return Err(Error::Owned);
        }
        // Cleanup exists before the FIRST resource write, including setup
        // readback failures. BAR type/prefetch attributes are preserved.
        let result = Self {
            saved: Some(Saved {
                device,
                offset,
                low,
                high,
                command,
            }),
            base,
            _view: core::marker::PhantomData,
        };
        device.write32(offset, base as u32 | (low & 15));
        if wide {
            device.write32(offset + 4, ((base as u64) >> 32) as u32);
        }
        device.write16(4, command | PCI_COMMAND_BITS::MEMORY_SPACE::SET.value);
        if device.memory_bar(offset) != Some(base as u64) || device.read16(4) & 7 != 2 {
            return Err(Error::Readback);
        }
        Ok(result)
    }
    pub(crate) fn registers(&self) -> &T {
        // SAFETY: bind's actual mapped, complete typed layout; this borrow
        // cannot outlive/close the owning resource lease.
        unsafe { &*(self.base as *const T) }
    }
    fn restore(saved: Saved) -> Result<(), Error> {
        saved.device.write16(4, saved.command & !7);
        saved.device.write32(saved.offset, saved.low);
        if let Some(high) = saved.high {
            saved.device.write32(saved.offset + 4, high);
        }
        saved.device.write16(4, saved.command);
        if saved.device.read32(saved.offset) != saved.low
            || saved
                .high
                .is_some_and(|high| saved.device.read32(saved.offset + 4) != high)
            || saved.device.read16(4) != saved.command
        {
            return Err(Error::Readback);
        }
        Ok(())
    }
    pub(crate) fn close(mut self) -> Result<(), Error> {
        Self::restore(self.saved.take().ok_or(Error::Owned)?)
    }
}
impl<T> Drop for TemporaryBar<T> {
    fn drop(&mut self) {
        if let Some(saved) = self.saved.take() {
            let _ = Self::restore(saved);
        }
    }
}
