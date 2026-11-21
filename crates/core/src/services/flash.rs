//! A bounded, writable region of the firmware flash.
//!
//! Data that firmware keeps across boots (memory-training caches, variable
//! stores) lives in a reserved region of the boot flash. Its driver binds
//! the region at construction and enforces the bounds, the part's geometry
//! and its protection; users see offsets relative to the region only.

use embedded_storage::nor_flash::{
    ErrorType, NorFlash, NorFlashError, NorFlashErrorKind, ReadNorFlash,
};

use super::ServiceError;

/// Read, program and erase within one reserved flash region. Every offset
/// is relative to the region; the implementation rejects anything outside
/// it and never unlocks unrelated parts of the flash.
pub trait FlashRegion {
    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), ServiceError>;
    /// Program erased bytes (NOR semantics: bits only go from 1 to 0).
    fn program(&mut self, offset: u32, bytes: &[u8]) -> Result<(), ServiceError>;
    /// Erase whole erase blocks of the part to `0xff`.
    fn erase(&mut self, offset: u32, length: u32) -> Result<(), ServiceError>;
}

/// Error returned by [`NorFlashAdapter`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NorFlashAdapterError {
    /// Access or erase range was not aligned to the configured geometry.
    NotAligned,
    /// Access extended beyond the configured region capacity or had `from > to`.
    OutOfBounds,
    /// Underlying [`FlashRegion`] operation failed.
    Service(ServiceError),
}

impl NorFlashError for NorFlashAdapterError {
    fn kind(&self) -> NorFlashErrorKind {
        match self {
            Self::NotAligned => NorFlashErrorKind::NotAligned,
            Self::OutOfBounds => NorFlashErrorKind::OutOfBounds,
            Self::Service(_) => NorFlashErrorKind::Other,
        }
    }
}

/// Bridges a [`FlashRegion`] to [`embedded_storage::nor_flash::ReadNorFlash`]
/// and [`embedded_storage::nor_flash::NorFlash`], enabling ecosystem NOR
/// storage crates (such as `sequential-storage`) on top of any fstart flash
/// driver.
///
/// The caller must supply the true hardware geometry (`ERASE_SIZE`,
/// `WRITE_SIZE`, `READ_SIZE`) and `capacity` of the underlying region:
/// `FlashRegion` itself does not expose geometry constants, so `ERASE_SIZE`
/// must match the region's erase granularity (e.g. 4 KiB on AMD FCH SPI,
/// 64 KiB on Intel ICH SPI), the region's physical base must be aligned to
/// `ERASE_SIZE`, and the driver must support `READ_SIZE`/`WRITE_SIZE`
/// granular transfers.
pub struct NorFlashAdapter<
    F,
    const ERASE_SIZE: usize,
    const WRITE_SIZE: usize = 1,
    const READ_SIZE: usize = 1,
> {
    region: F,
    capacity: u32,
}

impl<F, const ERASE_SIZE: usize, const WRITE_SIZE: usize, const READ_SIZE: usize>
    NorFlashAdapter<F, ERASE_SIZE, WRITE_SIZE, READ_SIZE>
{
    /// Wrap `region` with its byte `capacity` and compile-time geometry.
    ///
    /// # Panics
    ///
    /// Panics if any geometry constant is `0` or exceeds `u32::MAX`, or if
    /// `capacity` is not a multiple of `ERASE_SIZE`.
    #[must_use]
    pub const fn new(region: F, capacity: u32) -> Self {
        assert!(
            ERASE_SIZE > 0 && ERASE_SIZE <= u32::MAX as usize,
            "ERASE_SIZE must be in 1..=u32::MAX"
        );
        assert!(
            WRITE_SIZE > 0 && WRITE_SIZE <= u32::MAX as usize,
            "WRITE_SIZE must be in 1..=u32::MAX"
        );
        assert!(
            READ_SIZE > 0 && READ_SIZE <= u32::MAX as usize,
            "READ_SIZE must be in 1..=u32::MAX"
        );
        assert!(
            (capacity as usize).is_multiple_of(ERASE_SIZE),
            "flash region capacity must be a multiple of ERASE_SIZE"
        );
        Self { region, capacity }
    }

    /// Borrow the underlying [`FlashRegion`].
    #[must_use]
    pub const fn region(&self) -> &F {
        &self.region
    }

    /// Mutably borrow the underlying [`FlashRegion`].
    pub const fn region_mut(&mut self) -> &mut F {
        &mut self.region
    }

    /// Recover the underlying [`FlashRegion`].
    #[must_use]
    pub fn into_inner(self) -> F {
        self.region
    }

    fn check_bounds(&self, offset: u32, len: usize) -> Result<(), NorFlashAdapterError> {
        let len_u32 = u32::try_from(len).map_err(|_| NorFlashAdapterError::OutOfBounds)?;
        let end = offset
            .checked_add(len_u32)
            .ok_or(NorFlashAdapterError::OutOfBounds)?;
        if end > self.capacity {
            return Err(NorFlashAdapterError::OutOfBounds);
        }
        Ok(())
    }

    fn check_align(offset: u32, len: usize, unit: usize) -> Result<(), NorFlashAdapterError> {
        let unit_u32 = unit as u32;
        if !offset.is_multiple_of(unit_u32) || !len.is_multiple_of(unit) {
            return Err(NorFlashAdapterError::NotAligned);
        }
        Ok(())
    }
}

impl<F: FlashRegion, const ERASE_SIZE: usize, const WRITE_SIZE: usize, const READ_SIZE: usize>
    ErrorType for NorFlashAdapter<F, ERASE_SIZE, WRITE_SIZE, READ_SIZE>
{
    type Error = NorFlashAdapterError;
}

impl<F: FlashRegion, const ERASE_SIZE: usize, const WRITE_SIZE: usize, const READ_SIZE: usize>
    ReadNorFlash for NorFlashAdapter<F, ERASE_SIZE, WRITE_SIZE, READ_SIZE>
{
    const READ_SIZE: usize = READ_SIZE;

    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        self.check_bounds(offset, bytes.len())?;
        Self::check_align(offset, bytes.len(), READ_SIZE)?;
        if bytes.is_empty() {
            return Ok(());
        }
        self.region
            .read(offset, bytes)
            .map_err(NorFlashAdapterError::Service)
    }

    fn capacity(&self) -> usize {
        self.capacity as usize
    }
}

impl<F: FlashRegion, const ERASE_SIZE: usize, const WRITE_SIZE: usize, const READ_SIZE: usize>
    NorFlash for NorFlashAdapter<F, ERASE_SIZE, WRITE_SIZE, READ_SIZE>
{
    const WRITE_SIZE: usize = WRITE_SIZE;
    const ERASE_SIZE: usize = ERASE_SIZE;

    fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        if from > to || to > self.capacity {
            return Err(NorFlashAdapterError::OutOfBounds);
        }
        let erase_size = ERASE_SIZE as u32;
        if !from.is_multiple_of(erase_size) || !to.is_multiple_of(erase_size) {
            return Err(NorFlashAdapterError::NotAligned);
        }
        let len = to - from;
        if len == 0 {
            return Ok(());
        }
        self.region
            .erase(from, len)
            .map_err(NorFlashAdapterError::Service)
    }

    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        self.check_bounds(offset, bytes.len())?;
        Self::check_align(offset, bytes.len(), WRITE_SIZE)?;
        if bytes.is_empty() {
            return Ok(());
        }
        self.region
            .program(offset, bytes)
            .map_err(NorFlashAdapterError::Service)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MemFlash([u8; 8192]);

    impl MemFlash {
        fn new() -> Self {
            Self([0xFF; 8192])
        }
    }

    impl FlashRegion for MemFlash {
        fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), ServiceError> {
            let start = offset as usize;
            let end = start + bytes.len();
            bytes.copy_from_slice(&self.0[start..end]);
            Ok(())
        }

        fn program(&mut self, offset: u32, bytes: &[u8]) -> Result<(), ServiceError> {
            let start = offset as usize;
            for (slot, &b) in self.0[start..start + bytes.len()].iter_mut().zip(bytes) {
                *slot &= b;
            }
            Ok(())
        }

        fn erase(&mut self, offset: u32, length: u32) -> Result<(), ServiceError> {
            let start = offset as usize;
            self.0[start..start + length as usize].fill(0xFF);
            Ok(())
        }
    }

    #[test]
    fn nor_flash_adapter_roundtrip_and_bounds() {
        let mut flash = NorFlashAdapter::<_, 4096>::new(MemFlash::new(), 8192);
        assert_eq!(flash.capacity(), 8192);

        flash.write(16, b"fstart").unwrap();
        let mut buf = [0u8; 6];
        flash.read(16, &mut buf).unwrap();
        assert_eq!(&buf, b"fstart");

        // Empty operations at capacity boundary succeed.
        flash.read(8192, &mut []).unwrap();
        flash.write(8192, &[]).unwrap();
        flash.erase(8192, 8192).unwrap();

        // Unaligned erase and out-of-bounds access are rejected with the right kinds.
        assert_eq!(
            flash.erase(0, 1024).unwrap_err().kind(),
            NorFlashErrorKind::NotAligned
        );
        assert_eq!(
            flash.erase(4096, 0).unwrap_err().kind(),
            NorFlashErrorKind::OutOfBounds
        );
        assert_eq!(
            flash.read(8190, &mut buf).unwrap_err().kind(),
            NorFlashErrorKind::OutOfBounds
        );
        assert_eq!(
            flash.write(8190, b"1234").unwrap_err().kind(),
            NorFlashErrorKind::OutOfBounds
        );

        // Aligned erase clears the first 4 KiB sector back to 0xFF.
        flash.erase(0, 4096).unwrap();
        flash.read(16, &mut buf).unwrap();
        assert_eq!(buf, [0xFF; 6]);
    }

    #[test]
    fn nor_flash_adapter_enforces_write_alignment() {
        let mut flash = NorFlashAdapter::<_, 4096, 4>::new(MemFlash::new(), 8192);
        assert_eq!(
            flash.write(1, b"abcd").unwrap_err().kind(),
            NorFlashErrorKind::NotAligned
        );
        assert_eq!(
            flash.write(4, b"abc").unwrap_err().kind(),
            NorFlashErrorKind::NotAligned
        );
        flash.write(4, b"abcd").unwrap();
    }
}
