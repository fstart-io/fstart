//! What the SP3 SoCs keep from the OS in the memory map ([`crate::memory`]).

use crate::layout::Region;
use crate::memory::{MAX_RESERVATIONS, Reservation};
use crate::sp3::layout::LAYOUT;
use crate::sp3::silicon::Generation;
use fstart_core::services::memory_detect::E820Kind;
use heapless::Vec;

/// Legacy VGA/option ROM/BIOS area, reserved like AMI's MemDetect.
pub const LEGACY_HOLE: Region = Region::new(0xa_0000, 0x6_0000);
/// Page zero and the x86 low scratch (the arch crate's SIPI trampoline page
/// `0x8000` and S3/postcar stash): `[0, LOW_SCRATCH_END)`.
pub const LOW_RESERVED: Region = Region::new(0, 0xf000);

/// The reservations every SP3 boot keeps: page zero and low scratch, the
/// legacy hole, the APOB `[base, roundup(size, 4 KiB))`, the firmware's
/// DRAM with the reset image, and TSEG when SMM is set up. The APOB is
/// ACPI NVS on Rome/Milan as AGESA reports it, and reserved on Naples
/// (AGESA: boot-services data, i.e. RAM for the OS) because the firmware
/// may still read it after the payload starts.
pub fn reservations(
    generation: Generation,
    apob: Region,
    tseg: Option<Region>,
) -> Vec<Reservation, MAX_RESERVATIONS> {
    let apob_kind = match generation {
        Generation::Naples => E820Kind::Reserved,
        Generation::Rome | Generation::Milan => E820Kind::Nvs,
    };
    let apob = Region::new(apob.base, apob.size.next_multiple_of(0x1000));
    [
        Reservation::reserved(LOW_RESERVED),
        Reservation::reserved(LEGACY_HOLE),
        Reservation {
            span: apob,
            kind: apob_kind,
        },
        Reservation::reserved(LAYOUT.early),
    ]
    .into_iter()
    .chain(tseg.map(Reservation::reserved))
    .filter(|r| r.span.size != 0)
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn naples_apob_is_reserved_not_nvs() {
        let res = reservations(Generation::Naples, Region::new(0x3000_0000, 0x1000), None);
        assert_eq!(res.len(), 4);
        assert_eq!(res[2].kind, E820Kind::Reserved);
        assert_eq!(res[3].span, LAYOUT.early);
    }
}
