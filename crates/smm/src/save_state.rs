//! Volatile access to x86 SMM state-save areas.
//!
//! Hardware owns these bytes while an SMI is active. This module therefore
//! never forms Rust references or slices over a save-state area; every field
//! access is an explicit volatile byte transfer.

/// Offset of the revision word below the top of the SMBASE window. It is at
/// the same place in every Intel and AMD64 layout.
const REVISION_OFFSET: usize = 0x104;

/// Save-state layout selected by the CPU model.
#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum X86SaveStateFormat {
    /// Intel EM64T100/EM64T101 layout (revisions `0x30100`/`0x30101`). The
    /// SMBASE field is at the same offset in both.
    IntelEm64t,
    /// AMD64 layout (revision `0x20064`), as exposed by QEMU.
    Amd64,
}

impl X86SaveStateFormat {
    /// Human-readable layout name for logs.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::IntelEm64t => "Intel EM64T100/101",
            Self::Amd64 => "AMD64",
        }
    }

    const fn accepts(self, revision: u32) -> bool {
        match self {
            Self::IntelEm64t => matches!(revision, 0x0003_0100 | 0x0003_0101),
            Self::Amd64 => revision == 0x0002_0064,
        }
    }

    const fn smbase_offset(self) -> usize {
        match self {
            Self::IntelEm64t => 0x108,
            Self::Amd64 => 0x100,
        }
    }
}

/// Save-state access failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveStateError {
    /// The hardware revision does not match the selected format.
    RevisionMismatch {
        expected: X86SaveStateFormat,
        actual: u32,
    },
}

/// Raw volatile view of one CPU's state-save area.
#[derive(Debug, Clone, Copy)]
pub struct X86SaveState {
    top: *mut u8,
    format: X86SaveStateFormat,
}

impl X86SaveState {
    /// Build a view whose `top` is the exclusive top of the CPU's 64 KiB
    /// SMBASE window.
    ///
    /// # Safety
    ///
    /// `top` must identify a live, writable SMM state-save window for the
    /// duration of every operation on the returned value.
    #[inline(always)]
    pub const unsafe fn from_top(top: *mut u8, format: X86SaveStateFormat) -> Self {
        Self { top, format }
    }

    /// Read and validate the architectural save-state revision.
    #[inline(always)]
    pub fn revision(self) -> Result<u32, SaveStateError> {
        // SAFETY: `from_top` guarantees a live save-state window.
        let actual = unsafe { read_u32(self.top.sub(REVISION_OFFSET)) };
        if self.format.accepts(actual) {
            Ok(actual)
        } else {
            Err(SaveStateError::RevisionMismatch {
                expected: self.format,
                actual,
            })
        }
    }

    /// Write the relocated SMBASE after validating the hardware revision.
    /// Exactly one field is modified.
    #[inline(always)]
    pub fn write_smbase(self, smbase: u32) -> Result<(), SaveStateError> {
        self.revision()?;
        // SAFETY: `from_top` guarantees a live, writable save-state window.
        unsafe { write_u32(self.top.sub(self.format.smbase_offset()), smbase) };
        Ok(())
    }
}

#[inline(always)]
unsafe fn read_u32(ptr: *const u8) -> u32 {
    let mut bytes = [0u8; 4];
    for (i, byte) in bytes.iter_mut().enumerate() {
        *byte = unsafe { core::ptr::read_volatile(ptr.add(i)) };
    }
    u32::from_le_bytes(bytes)
}

#[inline(always)]
unsafe fn write_u32(ptr: *mut u8, value: u32) {
    for (i, byte) in value.to_le_bytes().iter().enumerate() {
        unsafe { core::ptr::write_volatile(ptr.add(i), *byte) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put_u32(memory: &mut [u8], offset: usize, value: u32) {
        memory[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    #[test]
    fn writes_only_selected_smbase_field() {
        let mut intel = [0x5au8; 0x400];
        put_u32(&mut intel, 0x400 - 0x104, 0x30101);
        let state = unsafe {
            X86SaveState::from_top(
                intel.as_mut_ptr().add(0x400),
                X86SaveStateFormat::IntelEm64t,
            )
        };
        state.write_smbase(0x1234_0000).unwrap();
        assert_eq!(
            &intel[0x400 - 0x108..0x400 - 0x104],
            &0x1234_0000u32.to_le_bytes()
        );
        assert_eq!(&intel[0x400 - 0x100..0x400 - 0xfc], &[0x5a; 4]);

        let mut amd64 = [0xa5u8; 0x200];
        put_u32(&mut amd64, 0x200 - 0x104, 0x20064);
        let state = unsafe {
            X86SaveState::from_top(amd64.as_mut_ptr().add(0x200), X86SaveStateFormat::Amd64)
        };
        state.write_smbase(0x5678_0000).unwrap();
        assert_eq!(
            &amd64[0x200 - 0x100..0x200 - 0xfc],
            &0x5678_0000u32.to_le_bytes()
        );
        assert_eq!(&amd64[0x200 - 0x108..0x200 - 0x104], &[0xa5; 4]);
    }

    #[test]
    fn revision_mismatch_changes_nothing() {
        let mut memory = [0x3cu8; 0x200];
        put_u32(&mut memory, 0x200 - 0x104, 0x20000);
        let before = memory;
        let state = unsafe {
            X86SaveState::from_top(memory.as_mut_ptr().add(0x200), X86SaveStateFormat::Amd64)
        };
        assert_eq!(
            state.write_smbase(0xdead_0000),
            Err(SaveStateError::RevisionMismatch {
                expected: X86SaveStateFormat::Amd64,
                actual: 0x20000,
            })
        );
        assert_eq!(memory, before);
    }
}
