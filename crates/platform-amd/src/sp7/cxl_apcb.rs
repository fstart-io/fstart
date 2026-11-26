// SPDX-License-Identifier: MIT OR Apache-2.0
//! The APCB the ABL booted with, read and rewritten at run time: the vendor
//! DXE's `CxlMgrApcbSetAndFlush` through AmdApcbDxeV4's write-back
//! (`AmdPspWriteBackApcbShadowCopy` `0x10FF0`, `PspUpdateFlash` `0xE320`;
//! reports/research/CXL_COMPLETE.md §2.1.3), on the SPI flash path fstart
//! keeps its settings with ([`fstart_driver_amd::fch::spi::flash`]).
//!
//! The vendor writes BIOS directory entry `0x60` instance 0 of the
//! directory the ABL booted only. fstart asks the PSP which ISH slot that
//! was (`GetActiveBootPartitionId`, C2P `0x36`; [`partition_slot`]) and
//! compares and rewrites that slot's `0x60/0` alone; the other slot keeps
//! its array, and is rewritten when the PSP boots it, as the vendor's. When
//! the PSP does not say, nothing is written ([`Booted::stale`]).
//!
//! A rewrite ([`rewrite`]) keeps the old bytes of every 4 KiB sector it
//! touches, verifies each written sector entirely and the whole range at
//! the end, and on any failure writes the old bytes back (verified too).
//! The caller resets only after a verified write.
//!
//! An edit bumps the unique instance, fixes the checksum and erases the
//! HMAC tail (`fstart_amdfw_format::apcb::set_cxl_array`). An APCB whose
//! tail carries an HMAC (none shipped does) is written only with a new one
//! the PSP signed and then validated ([`seal`]).

extern crate alloc;

use alloc::vec::Vec;

use fstart_amdfw_format::apcb::{self, CXL_ARRAY_SIZE, HMAC_SIZE};
use fstart_core::services::FlashRegion;

use super::flash::{ISH_A, ISH_B};

/// One APCB (`0x60/0`) as read from the flash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Slot {
    /// Flash offset and the entry's bytes.
    pub offset: u32,
    pub bytes: Vec<u8>,
    /// The ISH slots that name it (`super::flash::ISH_A`/`ISH_B`).
    pub ish: u8,
}

impl Slot {
    fn array(&self) -> Option<&[u8; CXL_ARRAY_SIZE]> {
        apcb::cxl_array(&self.bytes).ok().flatten()
    }
}

/// The ISH slot bit ([`ISH_A`], [`ISH_B`]) of the PSP's active boot
/// partition (`GetActiveBootPartitionId`): 0 slot A, 1 slot B. The id's
/// meaning is not established (WARM_RESET.md §2.5, [U]); any other value
/// is no answer. A wrong guess cannot make the boot use a wrong array: the
/// booted array must also match the ABL's windows
/// (`fstart_driver_amd::cxl::endpoint::consistent`) before a pool is
/// enabled or a slot written.
pub const fn partition_slot(id: u32) -> Option<u8> {
    match id {
        0 => Some(ISH_A),
        1 => Some(ISH_B),
        _ => None,
    }
}

/// The APCBs of the flash and the region array the ABL booted with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Booted {
    pub slots: Vec<Slot>,
    /// The slot the PSP booted, when it said ([`partition_slot`]).
    pub booted: Option<usize>,
    /// The booted slot's array; without one, the array every slot
    /// carries.
    pub array: [u8; CXL_ARRAY_SIZE],
}

impl Booted {
    /// From the APCB entries of the flash and the ISH slot the PSP booted
    /// (`ish`, [`partition_slot`]). `None` if no slot carries the CXL region
    /// array, or if the slots' arrays differ and the PSP did not say which
    /// it booted (the windows could be any slot's).
    pub fn new(slots: Vec<Slot>, ish: Option<u8>) -> Option<Self> {
        let booted = ish.and_then(|ish| {
            slots
                .iter()
                .position(|s| s.ish & ish != 0 && s.array().is_some())
        });
        let array = match booted {
            Some(i) => *slots[i].array()?,
            None => {
                let mut arrays = slots.iter().filter_map(Slot::array);
                let first = *arrays.next()?;
                if arrays.any(|a| *a != first) {
                    return None;
                }
                first
            }
        };
        Some(Self {
            slots,
            booted,
            array,
        })
    }

    /// The regions of the array.
    pub fn regions(
        &self,
    ) -> heapless::Vec<fstart_amdfw_format::apcb::CxlRegion, { apcb::MAX_CXL_REGIONS }> {
        fstart_driver_amd::cxl::apcb::parse(&self.array)
    }

    /// The slot to rewrite with `array`: the booted one, when its array
    /// differs. None when the PSP did not say which slot it booted.
    pub fn stale(&self, array: &[u8; CXL_ARRAY_SIZE]) -> Option<&Slot> {
        let slot = &self.slots[self.booted?];
        slot.array().is_some_and(|a| a != array).then_some(slot)
    }
}

/// Why an APCB was not rewritten.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Apcb(apcb::Error),
    /// The slot's APCB has no CXL region array.
    NoArray,
}

/// `slot`'s bytes with `array` written ([`apcb::set_cxl_array`]), and
/// whether its old tail held an HMAC (which the PSP must sign anew).
pub fn edit(slot: &[u8], array: &[u8; CXL_ARRAY_SIZE]) -> Result<(Vec<u8>, bool), Error> {
    let hmac = apcb::hmac_tail(slot)
        .map_err(Error::Apcb)?
        .is_some_and(apcb::hmac_used);
    let mut bytes = slot.to_vec();
    if !apcb::set_cxl_array(&mut bytes, array).map_err(Error::Apcb)? {
        return Err(Error::NoArray);
    }
    Ok((bytes, hmac))
}

/// A new HMAC for the edited APCB `bytes` (its tail erased by [`edit`])
/// from the PSP (`ApcbCalcCheckSumExtended`): `psp(buffer, size, op)` runs
/// C2P `0x34` with `op` on a copy of the APCB and its tail, `size` bytes of
/// APCB. Op 1 (validate) is established, op 0 (sign) is not
/// (CXL_COMPLETE.md §2.1.3): the tail is taken only if the PSP signed
/// (op 0) without touching the APCB, left a used tail ([`apcb::hmac_used`])
/// and then validated it (op 1). `false` otherwise, and `bytes` is kept.
pub fn seal(bytes: &mut [u8], mut psp: impl FnMut(&mut [u8], usize, u32) -> bool) -> bool {
    use fstart_driver_amd::psp::command::SignValidateHmac;
    let Ok(size) = apcb::size(bytes) else {
        return false;
    };
    let total = size + HMAC_SIZE;
    if bytes.len() < total {
        return false;
    }
    let mut buffer = bytes[..total].to_vec();
    let signed = psp(&mut buffer, size, SignValidateHmac::SIGN)
        && buffer[..size] == bytes[..size]
        && apcb::hmac_used(&buffer[size..])
        && psp(&mut buffer, size, SignValidateHmac::VALIDATE)
        && buffer[..size] == bytes[..size];
    if signed {
        bytes[size..total].copy_from_slice(&buffer[size..]);
    }
    signed
}

/// The erase sector of the SPI100 path
/// (`fstart_driver_amd::fch::spi::flash::ERASE_SIZE`, 4 KiB).
pub const ERASE_SIZE: u32 = 0x1000;
#[cfg(feature = "bundle")]
const _: () = assert!(ERASE_SIZE == fstart_driver_amd::fch::spi::flash::ERASE_SIZE);

/// The 4 KiB sectors `(offset, len)` covering `len` bytes at `offset`.
pub fn sectors(offset: u32, len: usize) -> (u32, u32) {
    let start = offset & !(ERASE_SIZE - 1);
    let end = (offset + len as u32).next_multiple_of(ERASE_SIZE);
    (start, end - start)
}

/// How a [`rewrite`] failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    /// The flash holds its old bytes: nothing was erased, or every sector
    /// written got its old bytes back, verified.
    NotWritten,
    /// The sector at this offset (in the range) holds neither its old nor
    /// its new bytes as far as the reads tell.
    Damaged(u32),
}

/// Attempts to put a sector's old bytes back.
const RESTORE_TRIES: usize = 2;

/// Erase, program and read back one sector: whether it now holds `data`.
fn put(flash: &mut impl FlashRegion, offset: u32, data: &[u8]) -> bool {
    let mut back = alloc::vec![0; data.len()];
    flash.erase(offset, ERASE_SIZE).is_ok()
        && flash.program(offset, data).is_ok()
        && flash.read(offset, &mut back).is_ok()
        && back == data
}

/// The old bytes back into every sector of `touched` (last first), each
/// read first and left alone if it already holds them. A sector that
/// cannot be restored does not stop the others; the first one is reported.
fn restore(flash: &mut impl FlashRegion, old: &[u8], touched: &[usize]) -> Failure {
    let sector = ERASE_SIZE as usize;
    let mut back = alloc::vec![0; sector];
    let mut outcome = Failure::NotWritten;
    for &i in touched.iter().rev() {
        let (offset, data) = ((i * sector) as u32, &old[i * sector..(i + 1) * sector]);
        if flash.read(offset, &mut back).is_ok() && back == data {
            continue;
        }
        if !(0..RESTORE_TRIES).any(|_| put(flash, offset, data)) && outcome == Failure::NotWritten {
            outcome = Failure::Damaged(offset);
        }
    }
    outcome
}

/// Write `bytes` at `at` into the `len` bytes of whole sectors `flash`
/// covers from its offset 0: read-modify-erase-program of the sectors
/// that change, each read back entirely, then the whole range read back.
/// On any failure every sector written is restored to its old bytes
/// ([`Failure`]). `Ok(false)` when nothing had to change.
pub fn rewrite(
    flash: &mut impl FlashRegion,
    len: u32,
    at: usize,
    bytes: &[u8],
) -> Result<bool, Failure> {
    let sector = ERASE_SIZE as usize;
    let len = len as usize;
    if !len.is_multiple_of(sector) || at.checked_add(bytes.len()).is_none_or(|end| end > len) {
        return Err(Failure::NotWritten);
    }
    let mut old = alloc::vec![0; len];
    flash.read(0, &mut old).map_err(|_| Failure::NotWritten)?;
    let mut new = old.clone();
    new[at..at + bytes.len()].copy_from_slice(bytes);
    let mut touched: Vec<usize> = Vec::new();
    for (i, (o, n)) in old.chunks(sector).zip(new.chunks(sector)).enumerate() {
        if o == n {
            continue;
        }
        touched.push(i);
        if !put(flash, (i * sector) as u32, n) {
            return Err(restore(flash, &old, &touched));
        }
    }
    let mut back = alloc::vec![0; len];
    if flash.read(0, &mut back).is_err() || back != new {
        return Err(restore(flash, &old, &touched));
    }
    Ok(!touched.is_empty())
}

#[cfg(all(feature = "bundle", target_os = "none"))]
pub(super) use run::{active_slot, read, write};

#[cfg(all(feature = "bundle", target_os = "none"))]
mod run {
    extern crate alloc;

    use fstart_driver_amd::ccx::tsc::Tsc;
    use fstart_driver_amd::fch::spi::smn::SmnFlash;
    use fstart_driver_amd::fch::{FchHw, Native};
    use fstart_driver_amd::psp::command::GetActiveBootPartitionId;

    use super::{Booted, Slot};
    use crate::sp7::Variant;
    use crate::sp7::init::State;

    /// The ISH slot the PSP booted (C2P `0x36`, [`super::partition_slot`]).
    pub fn active_slot<V: Variant>(state: &mut State, clock: &Tsc) -> Option<u8> {
        let answer = state
            .psp_mut()?
            .send_unchecked(clock, GetActiveBootPartitionId::new());
        let slot = answer
            .ok()
            .and_then(|buffer| super::partition_slot(buffer.request.id));
        if slot.is_none() {
            fstart_log::warn!("{}: the PSP did not say which ISH slot it booted", V::NAME);
        }
        slot
    }

    /// The APCBs of the flash through the SMN window, with the ISH slot the
    /// PSP booted (`ish`).
    pub fn read<V: Variant>(fch: &Native<Tsc>, ish: Option<u8>) -> Option<Booted> {
        let flash = SmnFlash::new(fch.smn())?;
        let size = flash.size().min(V::ROM_SIZE) as usize;
        let rd = |offset: usize, buf: &mut [u8]| {
            u32::try_from(offset).is_ok_and(|offset| flash.read(offset, buf).is_ok())
        };
        let ranges = crate::sp7::flash::apcb_ranges(size, rd).ok()?;
        let slots = ranges
            .into_iter()
            .filter_map(|range| {
                let mut bytes = alloc::vec![0; range.len];
                let offset = u32::try_from(range.offset).ok()?;
                flash.read(offset, &mut bytes).ok()?;
                Some(Slot {
                    offset,
                    bytes,
                    ish: range.ish,
                })
            })
            .collect();
        Booted::new(slots, ish)
    }

    pub use write::write;

    mod write {
        use fstart_driver_amd::ccx::tsc::Tsc;
        use fstart_driver_amd::fch::Native;
        use fstart_driver_amd::fch::spi::flash::{Extent, Flash};
        use fstart_driver_amd::psp::command::SignValidateHmac;

        use super::super::{Failure, Slot};
        use crate::sp7::Variant;
        use crate::sp7::init::State;

        /// C2P `0x34` with `op` on `bytes` (`size` bytes of APCB, its
        /// HMAC after them), through a DMA copy: `false` if the PSP did not
        /// complete it.
        fn hmac(state: &mut State, clock: &Tsc, bytes: &mut [u8], size: usize, op: u32) -> bool {
            let total = bytes.len();
            let Ok(buffer) = crate::memory::dma_alloc(total, 0x1000) else {
                return false;
            };
            buffer.copy_from_slice(bytes);
            // SAFETY: the buffer is ours; the PSP reads it from DRAM.
            unsafe { fstart_arch::x86::writeback_cache_range(buffer.as_ptr(), total) };
            let request = SignValidateHmac::new(buffer.as_ptr() as u64, size as u32, op);
            let Some(psp) = state.psp_mut() else {
                return false;
            };
            if psp.send_unchecked(clock, request).is_err() {
                return false;
            }
            // SAFETY: as above; flushed again so no line cached before the
            // PSP's write serves the copy.
            unsafe { fstart_arch::x86::writeback_cache_range(buffer.as_ptr(), total) };
            bytes.copy_from_slice(buffer);
            true
        }

        /// Rewrite `slot` with `array`: [`super::super::rewrite`] of its
        /// 4 KiB sectors through `fch`'s SPI100. The vendor's
        /// AmdPspFlashAccLib writes there too, without asking the PSP
        /// (reports/research/SPI_ARBITRATION.md).
        pub fn write<V: Variant>(
            state: &mut State,
            fch: &Native<Tsc>,
            clock: &Tsc,
            slot: &Slot,
            array: &[u8; fstart_amdfw_format::apcb::CXL_ARRAY_SIZE],
        ) -> Result<(), Failure> {
            let Ok((mut bytes, signed)) = super::super::edit(&slot.bytes, array) else {
                return Err(Failure::NotWritten);
            };
            if signed
                && !super::super::seal(&mut bytes, |buffer, size, op| {
                    hmac(state, clock, buffer, size, op)
                })
            {
                fstart_log::error!(
                    "{}: the APCB at {:#x} has an HMAC the PSP did not sign and validate anew: not written",
                    V::NAME,
                    slot.offset
                );
                return Err(Failure::NotWritten);
            }
            let (start, len) = super::super::sectors(slot.offset, slot.bytes.len());
            let extent = Extent {
                offset: start,
                size: len,
            };
            // SAFETY: socket 0's FCH decodes SPI100 at its fixed base; the BSP
            // owns the command engine on x86 before the payload, which binds
            // it only later; the code runs from DRAM.
            let Ok(mut flash) =
                (unsafe { Flash::new(fch, *clock, extent, slot.offset, &slot.bytes) })
            else {
                return Err(Failure::NotWritten);
            };
            if !flash.writable() {
                return Err(Failure::NotWritten);
            }
            let at = (slot.offset - start) as usize;
            match super::super::rewrite(&mut flash, len, at, &bytes) {
                Ok(_) => Ok(()),
                Err(Failure::Damaged(offset)) => Err(Failure::Damaged(start + offset)),
                Err(failure) => Err(failure),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use fstart_amdfw_format::apcb::{CxlMemory, CxlRegion};
    use fstart_core::services::ServiceError;

    /// An APCB entry with the DF group's zeroed 0xCC array, in a 0x1000
    /// entry with an erased tail (as GIGABYTE's `0x60/0`).
    fn entry() -> Vec<u8> {
        let mut apcb = Vec::new();
        apcb.extend_from_slice(b"APCB");
        apcb.extend_from_slice(&0x80u16.to_le_bytes());
        apcb.extend_from_slice(&0x30u16.to_le_bytes());
        apcb.extend_from_slice(&0u32.to_le_bytes());
        apcb.extend_from_slice(&7u32.to_le_bytes());
        apcb.resize(0x80, 0);
        let df = 16 + 16 + 0x540;
        apcb.extend_from_slice(b"DFG ");
        apcb.extend_from_slice(&0x1703u16.to_le_bytes());
        apcb.extend_from_slice(&16u16.to_le_bytes());
        apcb.extend_from_slice(&1u16.to_le_bytes());
        apcb.extend_from_slice(&0u16.to_le_bytes());
        apcb.extend_from_slice(&(df as u32).to_le_bytes());
        apcb.extend_from_slice(&[
            0x03, 0x17, 0xcc, 0x00, 0x50, 0x05, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x00,
            0xff, 0xff,
        ]);
        apcb.resize(0x80 + df, 0);
        let size = apcb.len() as u32;
        apcb[8..12].copy_from_slice(&size.to_le_bytes());
        apcb.resize(0x1000, 0xff);
        apcb::fix_checksum(&mut apcb).unwrap();
        apcb
    }

    fn new_array() -> [u8; CXL_ARRAY_SIZE] {
        let region = CxlRegion {
            size: 16 << 30,
            alignment: 0x20,
            socket: 0,
            die: 0,
            nbio_map: 2,
            interleave_size: 0xff,
            sub_interleave_map: [1; 4],
            sub_interleave_size: 0xff,
            memory: CxlMemory::Volatile,
            device_type: 3,
            link_speed: 5,
            flit: 0,
            segment: 0,
            bus: 0x41,
            device: 0,
            function: 0,
            status: 0,
        };
        apcb::cxl_array_of(&[region]).unwrap()
    }

    fn slot(offset: u32, bytes: Vec<u8>, ish: u8) -> Slot {
        Slot { offset, bytes, ish }
    }

    #[test]
    fn only_the_booted_slot_is_compared_and_rewritten() {
        let new = new_array();
        let mut b = entry();
        apcb::set_cxl_array(&mut b, &new).unwrap();
        let slots = std::vec![slot(0x163e000, entry(), ISH_A), slot(0x1e1e000, b, ISH_B)];
        // The PSP booted A: A's (zeroed) array, A is stale.
        let booted = Booted::new(slots.clone(), Some(ISH_A)).unwrap();
        assert_eq!(
            (booted.booted, booted.array),
            (Some(0), [0; CXL_ARRAY_SIZE])
        );
        assert!(booted.regions().iter().all(|r| r.size == 0));
        assert_eq!(booted.stale(&new).map(|s| s.offset), Some(0x163e000));
        // It booted B: B's array, which is up to date; A is left alone.
        let booted = Booted::new(slots.clone(), Some(ISH_B)).unwrap();
        assert_eq!((booted.booted, booted.array), (Some(1), new));
        assert_eq!(booted.stale(&new), None);
        assert_eq!(
            booted.stale(&[0; CXL_ARRAY_SIZE]).map(|s| s.offset),
            Some(0x1e1e000)
        );
        // No answer: the windows could be either slot's; nothing is used.
        assert_eq!(Booted::new(slots.clone(), None), None);
        // No answer but one array: it is used, and nothing is written.
        let same = std::vec![
            slot(0x163e000, entry(), ISH_A),
            slot(0x1e1e000, entry(), ISH_B)
        ];
        let booted = Booted::new(same, None).unwrap();
        assert_eq!((booted.booted, booted.stale(&new)), (None, None));
        // One APCB for both slots.
        let shared = Booted::new(
            std::vec![slot(0x163e000, entry(), ISH_A | ISH_B)],
            Some(ISH_B),
        )
        .unwrap();
        assert_eq!(shared.booted, Some(0));
        assert_eq!(
            (partition_slot(0), partition_slot(1)),
            (Some(ISH_A), Some(ISH_B))
        );
        assert_eq!(partition_slot(u32::MAX), None);
    }

    #[test]
    fn an_edit_seals_and_flags_an_hmac() {
        let new = new_array();
        let (edited, hmac) = edit(&entry(), &new).unwrap();
        assert!(!hmac);
        assert!(apcb::checksum_ok(&edited).unwrap());
        assert_eq!(&edited[0x0c..0x10], &8u32.to_le_bytes());
        assert_eq!(apcb::cxl_array(&edited).unwrap().unwrap(), &new);
        // A signed APCB is flagged for the PSP.
        let mut signed = entry();
        let size = apcb::size(&signed).unwrap();
        signed[size] = 0x12;
        assert!(edit(&signed, &new).unwrap().1);
        assert_eq!(edit(b"APCB", &new), Err(Error::Apcb(apcb::Error::Header)));
        // Dell's 0x60/0 is not sector aligned.
        assert_eq!(sectors(0x161c054, 0x600), (0x161c000, 0x1000));
        assert_eq!(sectors(0x161cf00, 0x600), (0x161c000, 0x2000));
    }

    #[test]
    fn an_hmac_is_kept_only_when_the_psp_signs_and_validates_it() {
        use fstart_driver_amd::psp::command::SignValidateHmac;
        let (edited, _) = edit(&entry(), &new_array()).unwrap();
        let size = apcb::size(&edited).unwrap();
        let sign = |buffer: &mut [u8], size: usize, op: u32| {
            if op == SignValidateHmac::SIGN {
                for (i, b) in buffer[size..].iter_mut().enumerate() {
                    *b = i as u8;
                }
            }
            true
        };
        let mut bytes = edited.clone();
        assert!(seal(&mut bytes, sign));
        assert_eq!(bytes[size + 5], 5);
        assert_eq!(bytes[..size], edited[..size]);
        // The PSP fails, signs nothing, refuses validation, or touches the
        // APCB: the tail stays erased and the caller does not write.
        type Psp<'a> = &'a dyn Fn(&mut [u8], usize, u32) -> bool;
        let cases: [Psp<'_>; 4] = [
            &|_, _, _| false,
            &|_, _, _| true,
            &|b, s, op| op == SignValidateHmac::SIGN && sign(b, s, op),
            &|b, s, op| {
                b[0x20] ^= 1;
                sign(b, s, op)
            },
        ];
        for psp in cases {
            let mut bytes = edited.clone();
            assert!(!seal(&mut bytes, psp));
            assert_eq!(bytes, edited);
        }
    }

    /// A NOR flash that fails as told, counting calls from 1: the `n`-th
    /// erase refuses, the `n`-th program writes half its bytes and fails
    /// or reports success with a bit dropped, the `n`-th read fails.
    #[derive(Default)]
    struct Model {
        bytes: Vec<u8>,
        erases: usize,
        programs: usize,
        reads: usize,
        fail_erase: Vec<usize>,
        fail_program: Vec<usize>,
        corrupt_program: Vec<usize>,
        fail_read: Vec<usize>,
        /// Every program from this one on fails.
        broken_from: Option<usize>,
    }

    impl Model {
        fn new(bytes: Vec<u8>) -> Self {
            Self {
                bytes,
                ..Self::default()
            }
        }
    }

    impl FlashRegion for Model {
        fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), ServiceError> {
            self.reads += 1;
            if self.fail_read.contains(&self.reads) {
                return Err(ServiceError::HardwareError);
            }
            let at = offset as usize;
            bytes.copy_from_slice(&self.bytes[at..at + bytes.len()]);
            Ok(())
        }

        fn program(&mut self, offset: u32, bytes: &[u8]) -> Result<(), ServiceError> {
            self.programs += 1;
            let n = self.programs;
            let at = offset as usize;
            let broken = self.broken_from.is_some_and(|from| n >= from);
            let len = if self.fail_program.contains(&n) || broken {
                bytes.len() / 2
            } else {
                bytes.len()
            };
            for (b, &d) in self.bytes[at..at + len].iter_mut().zip(bytes) {
                *b &= d;
            }
            if len != bytes.len() {
                return Err(ServiceError::HardwareError);
            }
            if self.corrupt_program.contains(&n) {
                self.bytes[at + 7] &= !0x10;
            }
            Ok(())
        }

        fn erase(&mut self, offset: u32, length: u32) -> Result<(), ServiceError> {
            self.erases += 1;
            if self.fail_erase.contains(&self.erases) {
                return Err(ServiceError::HardwareError);
            }
            self.bytes[offset as usize..(offset + length) as usize].fill(0xff);
            Ok(())
        }
    }

    /// Dell's unaligned `0x60/0`: 0x600 bytes at `0x...F00` in two
    /// sectors, a neighbour's bytes around it.
    fn dell() -> (Vec<u8>, usize, Vec<u8>) {
        let mut flash: Vec<u8> = (0..0x2000).map(|i| (i * 7) as u8).collect();
        let at = 0xf00;
        let (apcb, _) = edit(&entry(), &new_array()).unwrap();
        let apcb = apcb[..0x600].to_vec();
        flash[at..at + 0x600].fill(0x5a);
        (flash, at, apcb)
    }

    #[test]
    fn a_verified_rewrite_keeps_the_neighbours() {
        let (old, at, apcb) = dell();
        let mut m = Model::new(old.clone());
        assert_eq!(rewrite(&mut m, 0x2000, at, &apcb), Ok(true));
        assert_eq!(&m.bytes[at..at + apcb.len()], &apcb[..]);
        assert_eq!(&m.bytes[..at], &old[..at]);
        assert_eq!(&m.bytes[at + apcb.len()..], &old[at + apcb.len()..]);
        assert_eq!(m.erases, 2);
        // Again: nothing changes, nothing is erased.
        assert_eq!(rewrite(&mut m, 0x2000, at, &apcb), Ok(false));
        assert_eq!(m.erases, 2);
        // Out of range or not whole sectors: refused untouched.
        assert_eq!(
            rewrite(&mut m, 0x2000, 0x1c00, &apcb),
            Err(Failure::NotWritten)
        );
        assert_eq!(rewrite(&mut m, 0x1800, 0, &apcb), Err(Failure::NotWritten));
        assert_eq!(m.erases, 2);
    }

    #[test]
    fn a_failed_rewrite_puts_the_old_bytes_back() {
        let (old, at, apcb) = dell();
        let cases: [fn(&mut Model); 6] = [
            // Erase refused: nothing changed.
            |m| m.fail_erase = std::vec![1],
            // Erase done, program fails halfway in the first sector.
            |m| m.fail_program = std::vec![1],
            // Program reports success, the read back differs.
            |m| m.corrupt_program = std::vec![1],
            // The first sector written and verified, the second fails: the
            // first goes back too.
            |m| m.fail_program = std::vec![2],
            // The second erase refused after the first sector's write.
            |m| m.fail_erase = std::vec![2],
            // The final read of the whole range fails.
            |m| m.fail_read = std::vec![4],
        ];
        for (i, fault) in cases.iter().enumerate() {
            let mut m = Model::new(old.clone());
            fault(&mut m);
            assert_eq!(
                rewrite(&mut m, 0x2000, at, &apcb),
                Err(Failure::NotWritten),
                "case {i}"
            );
            assert_eq!(m.bytes, old, "case {i}");
        }
        // The first read fails: nothing is erased.
        let mut m = Model::new(old.clone());
        m.fail_read = std::vec![1];
        assert_eq!(rewrite(&mut m, 0x2000, at, &apcb), Err(Failure::NotWritten));
        assert_eq!(m.erases, 0);
    }

    #[test]
    fn a_flash_that_cannot_be_restored_is_reported() {
        let (old, at, apcb) = dell();
        // The second sector fails and every program after it: neither
        // sector gets its old bytes back; the second is reported.
        let mut m = Model::new(old.clone());
        m.broken_from = Some(2);
        assert_eq!(
            rewrite(&mut m, 0x2000, at, &apcb),
            Err(Failure::Damaged(0x1000))
        );
        assert_eq!(m.programs, 2 + 2 * RESTORE_TRIES);
        // Only the failing sector: it is the damaged one.
        let mut m = Model::new(old);
        m.broken_from = Some(1);
        assert_eq!(rewrite(&mut m, 0x2000, at, &apcb), Err(Failure::Damaged(0)));
    }
}
