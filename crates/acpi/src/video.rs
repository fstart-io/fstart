// SPDX-License-Identifier: MIT OR Apache-2.0
//! ACPI video extensions, independent of the graphics vendor and ROM executor.
//!
//! ACPI 6.6 Appendix B.4.3, `_ROM (Get ROM Data)`:
//! <https://uefi.org/specs/ACPI/6.6/Apx_B_Video_Extensions.html#rom-get-rom-data>.

use crate::{
    Aml, AmlError,
    aml::{
        Arg, BufferData, BufferTerm, CreateField, Equal, GreaterEqual, GreaterThan, If, Local,
        Method, Multiply, Name, Path, Return, Store, Subtract,
    },
};
use alloc::{vec, vec::Vec};

/// Emit `_ROM` and its private `VDAT` buffer inside a display device's scope.
///
/// The caller supplies the complete trusted image (including any POST patches)
/// and places this fragment under the PCI display function's ACPI companion,
/// not its host bridge or an output connector. No `_HID` or GPU policy is added.
///
/// The bytes are embedded in AML: after table assembly neither the source slice
/// nor emulator RAM needs to survive. The platform must retain the ACPI tables
/// as usual. There are no identity-mapping or firmware-heap lifetime assumptions.
///
/// Reads are capped at 4096 bytes. Like coreboot, reads crossing or beyond the
/// end return a zero-padded buffer of the capped requested length. Zero-length
/// reads return an empty buffer. Offsets are checked before subtraction or bit
/// conversion; no ROM alignment or minimum-4-KiB assumption is made.
///
/// `VDAT` is reserved in the containing scope. Temporary named buffer/field
/// objects require serialization; each display device has its own method lock.
pub fn rom_aml(rom: &[u8]) -> Result<Vec<u8>, AmlError> {
    if rom.is_empty() {
        return Err(AmlError::InvalidRange);
    }
    // AML PkgLength has 28 bits, including its own encoding. Leave room for
    // the buffer's size operand and headers before using the upstream emitter.
    if rom.len() > 0x0fff_ffe0 {
        return Err(AmlError::LengthOverflow);
    }
    let length = rom.len() as u64;
    let mut bytes = Vec::new();
    Name::new("VDAT".into(), &BufferData::new(rom.to_vec())).to_aml_bytes(&mut bytes);
    Method::new(
        "_ROM".into(),
        2,
        true,
        vec![
            &Store::new(&Local(0), &Arg(1)),
            &If::new(
                &GreaterThan::new(&Local(0), &4096u32),
                vec![&Store::new(&Local(0), &4096u32)],
            ),
            &Name::new("RBUF".into(), &BufferTerm::new(&Local(0))),
            &If::new(
                &Equal::new(&Local(0), &0u8),
                vec![&Return::new(&Path::new("RBUF"))],
            ),
            &If::new(
                &GreaterEqual::new(&Arg(0), &length),
                vec![&Return::new(&Path::new("RBUF"))],
            ),
            &Subtract::new(&Local(1), &length, &Arg(0)),
            &If::new(
                &GreaterThan::new(&Local(1), &Local(0)),
                vec![&Store::new(&Local(1), &Local(0))],
            ),
            &Multiply::new(&Local(2), &Arg(0), &8u8),
            &Multiply::new(&Local(1), &Local(1), &8u8),
            &CreateField::new(&Path::new("RFIL"), &Path::new("VDAT"), &Local(2), &Local(1)),
            &Store::new(&Path::new("RBUF"), &Path::new("RFIL")),
            &Return::new(&Path::new("RBUF")),
        ],
    )
    .to_aml_bytes(&mut bytes);
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_images_are_not_advertised() {
        assert_eq!(rom_aml(&[]), Err(AmlError::InvalidRange));
    }

    #[test]
    fn source_is_copied_and_method_is_serialized_with_two_arguments() {
        let mut rom = vec![0x55, 0xaa, 0x37, 0x91, 0x62, 0xf0, 0x18];
        let aml = rom_aml(&rom).unwrap();
        assert!(aml.windows(rom.len()).any(|w| w == rom));
        rom.fill(0);
        assert!(aml.windows(5).any(|w| w == b"_ROM\x0a"));
        assert!(!aml.windows(rom.len()).any(|w| w == rom));
    }
}
