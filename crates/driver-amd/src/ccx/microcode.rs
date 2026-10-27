// SPDX-License-Identifier: MIT OR Apache-2.0
//! Flat Family 17h microcode patches (the vendor BIOS directory 0x66 entries
//! and the Picasso .bin files), not AMD container or signed-wrapper parsing.
//! Each SoC names its CPU signatures and patch budget with a [`Family`].
use core::marker::PhantomData;
use fstart_arch::x86::cpu::msr::{Msr, MsrAccess, PATCH_LEVEL};
use tock_registers::interfaces::{Readable, Writeable};
use zerocopy::{
    FromBytes, Immutable, KnownLayout,
    byteorder::{LittleEndian, U16, U32},
};

/// Every supported vendor patch is 0xc80 bytes. This deliberately bounded
/// format does not accept other families/wrappers.
pub const PATCH_BYTES: usize = 3200;
/// Writing a patch's physical address loads it.
const PATCH_LOADER: Msr = Msr::new(0xc001_0020);

/// The CPUs a SoC's patches are for.
pub trait Family {
    /// CPUID(1).EAX of the supported models, stepping masked off.
    const SIGNATURES: &'static [u32];
    /// Firmware input budget, not silicon discovery.
    const MAX_PATCHES: usize;
}

#[repr(C)]
#[derive(FromBytes, Immutable, KnownLayout)]
struct Header {
    date_code: U32<LittleEndian>,
    patch_id: U32<LittleEndian>,
    patch_data_id: U16<LittleEndian>,
    reserved1: [u8; 6],
    chipset1: U32<LittleEndian>,
    chipset2: U32<LittleEndian>,
    processor_rev: U16<LittleEndian>,
    chipset1_rev: u8,
    chipset2_rev: u8,
    reserved2: [u8; 4],
}
const _: () = assert!(core::mem::size_of::<Header>() == 32);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Format,
    CpuIdentity,
    ProcessorRevision { expected: u16, actual: u16 },
    Address,
    Rejected { expected: u32, actual: u32 },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    AlreadyCurrent(u32),
    Installed(u32),
}

/// Concatenated unchanged flat inputs. No wrapper, implicit best-version choice,
/// duplicate equivalent revisions or ignored malformed non-selected entries.
/// Format validation and exact selection are NOT authentication/freshness proof.
pub struct Bundle<'a, F> {
    bytes: &'a [u8],
    family: PhantomData<F>,
}
impl<'a, F: Family> Bundle<'a, F> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self, Error> {
        if bytes.is_empty()
            || !bytes.len().is_multiple_of(PATCH_BYTES)
            || bytes.len() / PATCH_BYTES > F::MAX_PATCHES
        {
            return Err(Error::Format);
        }
        let patches = bytes.as_chunks::<PATCH_BYTES>().0;
        for (index, patch) in patches.iter().enumerate() {
            let revision = header(patch)?.processor_rev.get();
            let duplicate = patches[..index]
                .iter()
                .any(|other| header(other).is_ok_and(|h| h.processor_rev.get() == revision));
            if revision == 0 || duplicate {
                return Err(Error::Format);
            }
        }
        Ok(Self {
            bytes,
            family: PhantomData,
        })
    }
    /// The patch whose equivalence ID matches `signature`.
    pub fn select(&self, signature: u32) -> Result<Patch<'a, F>, Error> {
        for bytes in self.bytes.as_chunks::<PATCH_BYTES>().0 {
            match Patch::parse(bytes, signature) {
                Ok(patch) => return Ok(patch),
                Err(Error::ProcessorRevision { .. }) => {}
                Err(error) => return Err(error),
            }
        }
        Err(Error::CpuIdentity)
    }
}

fn header(bytes: &[u8]) -> Result<Header, Error> {
    if bytes.len() != PATCH_BYTES {
        return Err(Error::Format);
    }
    let (header, _) = Header::read_from_prefix(bytes).map_err(|_| Error::Format)?;
    if header.patch_id.get() == 0 || header.chipset1.get() != 0 || header.chipset2.get() != 0 {
        return Err(Error::Format);
    }
    Ok(header)
}

/// The equivalence ID of a CPUID(1).EAX signature (e.g. 0x8012 for ZP-B2).
pub const fn equivalent_revision(signature: u32) -> u16 {
    ((signature & 0xff0000) >> 8 | (signature & 0xff)) as u16
}

/// An immutable vendor patch view. Parsing is NOT signature authentication.
pub struct Patch<'a, F> {
    bytes: &'a [u8],
    signature: u32,
    revision: u32,
    family: PhantomData<F>,
}
impl<'a, F: Family> Patch<'a, F> {
    pub fn parse(bytes: &'a [u8], signature: u32) -> Result<Self, Error> {
        if !F::SIGNATURES.contains(&(signature & !0xf)) {
            return Err(Error::CpuIdentity);
        }
        let header = header(bytes)?;
        let expected = equivalent_revision(signature);
        let actual = header.processor_rev.get();
        if actual != expected {
            return Err(Error::ProcessorRevision { expected, actual });
        }
        Ok(Self {
            bytes,
            signature,
            revision: header.patch_id.get(),
            family: PhantomData,
        })
    }
    pub fn bytes(&self) -> &'a [u8] {
        self.bytes
    }
    pub fn revision(&self) -> u32 {
        self.revision
    }

    /// Validate the whole below-4GiB, 16-byte-aligned physical read extent.
    /// Numeric geometry alone does not prove it addresses the parsed bytes.
    pub fn validate_address(address: u64) -> Result<(), Error> {
        if address == 0
            || address & 15 != 0
            || address
                .checked_add(PATCH_BYTES as u64)
                .is_none_or(|end| end > 1 << 32)
        {
            return Err(Error::Address);
        }
        Ok(())
    }

    /// Load once and verify the authoritative installed patch level. A newer
    /// installed revision is retained, never downgraded. No rejected-load retry.
    ///
    /// # Safety
    /// Supported CPU at ring 0; serialize all CPU/sibling patch operations and
    /// exclude asynchronous firmware work. `address` must address these exact
    /// trusted vendor bytes in coherent, CPU-readable physical memory. Keep the
    /// complete allocation immutable, mapped and reserved through reset. Do not
    /// pass a flash-XIP/virtual-only address or assume parsing authenticates it.
    /// A fault or rejected submitted load requires platform fail-stop/reset.
    pub unsafe fn apply(
        &self,
        cpu: &impl MsrAccess,
        address: u64,
        signature: u32,
    ) -> Result<Outcome, Error> {
        if signature != self.signature {
            return Err(Error::CpuIdentity);
        }
        Self::validate_address(address)?;
        let current = cpu.reg(PATCH_LEVEL).get() as u32;
        if current >= self.revision {
            return Ok(Outcome::AlreadyCurrent(current));
        }
        cpu.reg(PATCH_LOADER).set(address);
        let actual = cpu.reg(PATCH_LEVEL).get() as u32;
        if actual != self.revision {
            return Err(Error::Rejected {
                expected: self.revision,
                actual,
            });
        }
        Ok(Outcome::Installed(actual))
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use core::cell::RefCell;
    use fstart_arch::x86::cpu::msr::testing::MsrModel;
    use std::vec::Vec;
    // The Picasso PCO B0/B1 and RV2 A0 inputs.
    enum Test {}
    impl Family for Test {
        const SIGNATURES: &'static [u32] = &[0x00810f10, 0x00810f80, 0x00820f00];
        const MAX_PATCHES: usize = 3;
    }
    const MAX_PATCHES: usize = Test::MAX_PATCHES;
    type Bundle<'a> = super::Bundle<'a, Test>;
    type Patch<'a> = super::Patch<'a, Test>;
    struct Cpu {
        level: u32,
        loaded_level: u32,
        writes: Vec<(u32, u64)>,
    }
    impl MsrModel for Cpu {
        fn read(&mut self, index: u32) -> u64 {
            assert_eq!(index, PATCH_LEVEL.index());
            u64::from(self.level)
        }
        fn write(&mut self, index: u32, value: u64) {
            assert_eq!(index, PATCH_LOADER.index());
            self.writes.push((index, value));
            self.level = self.loaded_level;
        }
    }
    fn bytes(signature: u32, revision: u32) -> [u8; PATCH_BYTES] {
        let mut bytes = [0; PATCH_BYTES];
        bytes[4..8].copy_from_slice(&revision.to_le_bytes());
        bytes[24..26].copy_from_slice(&equivalent_revision(signature).to_le_bytes());
        bytes
    }
    #[test]
    fn bundle_preflights_every_entry_and_selects_without_fallback() {
        let a = bytes(0x00810f80, 0x08108002);
        let b = bytes(0x00820f01, 0x08200105);
        let mut data = Vec::from(a);
        data.extend_from_slice(&b);
        let bundle = Bundle::parse(&data).unwrap();
        assert_eq!(bundle.select(0x00820f01).unwrap().bytes(), b);
        assert!(bundle.select(0x00810f81).is_err());
        assert!(bundle.select(0x000106c2).is_err());
        data[PATCH_BYTES + 16] = 1;
        assert!(Bundle::parse(&data).is_err()); // even when selecting valid entry0
        data[PATCH_BYTES..].copy_from_slice(&a);
        assert!(Bundle::parse(&data).is_err()); // no ambiguous version choice
        assert!(Bundle::parse(&[]).is_err());
        assert!(Bundle::parse(&data[..data.len() - 1]).is_err());
        assert!(Bundle::parse(&[0; PATCH_BYTES * (MAX_PATCHES + 1)]).is_err());
    }
    #[test]
    fn flat_patch_selection_rejects_wrong_cpu_length_and_chipset() {
        let bytes = bytes(0x00810f81, 0x0810810e);
        assert_eq!(equivalent_revision(0x00820f01), 0x8201);
        assert!(Patch::parse(&bytes, 0x00810f81).is_ok());
        assert!(matches!(
            Patch::parse(&bytes, 0x00810f80),
            Err(Error::ProcessorRevision { .. })
        ));
        assert!(matches!(
            Patch::parse(&bytes[..3199], 0x00810f81),
            Err(Error::Format)
        ));
        assert!(matches!(
            Patch::parse(&bytes, 0x000906a0),
            Err(Error::CpuIdentity)
        ));
        let mut bad = bytes;
        bad[16] = 1;
        assert!(matches!(Patch::parse(&bad, 0x00810f81), Err(Error::Format)));
    }
    #[test]
    fn loader_has_no_downgrade_retry_or_preflight_writes() {
        let bytes = bytes(0x00810f81, 0x0810810e);
        let patch = Patch::parse(&bytes, 0x00810f81).unwrap();
        let cpu = RefCell::new(Cpu {
            level: 1,
            loaded_level: patch.revision(),
            writes: Vec::new(),
        });
        let writes = || core::mem::take(&mut cpu.borrow_mut().writes);
        // Mock transport only: the numeric address is never dereferenced.
        assert_eq!(
            unsafe { patch.apply(&cpu, 0x2070000, 0x00810f81) },
            Ok(Outcome::Installed(patch.revision()))
        );
        assert_eq!(writes(), [(PATCH_LOADER.index(), 0x2070000)]);
        cpu.borrow_mut().level += 1;
        assert_eq!(
            unsafe { patch.apply(&cpu, 0x2070000, 0x00810f81) },
            Ok(Outcome::AlreadyCurrent(cpu.borrow().level))
        );
        assert!(writes().is_empty());
        cpu.borrow_mut().level = 0;
        cpu.borrow_mut().loaded_level = 0;
        assert_eq!(
            unsafe { patch.apply(&cpu, 0x2070000, 0x00810f81) },
            Err(Error::Rejected {
                expected: patch.revision(),
                actual: 0
            })
        );
        assert_eq!(writes().len(), 1);
        for address in [0, 0x2070001, (1 << 32) - 16, u64::MAX - 15] {
            assert_eq!(
                unsafe { patch.apply(&cpu, address, 0x00810f81) },
                Err(Error::Address)
            );
        }
        assert_eq!(
            unsafe { patch.apply(&cpu, 0x2070000, 0x00810f80) },
            Err(Error::CpuIdentity)
        );
        assert!(writes().is_empty());
    }
}
