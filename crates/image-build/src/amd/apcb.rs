// SPDX-License-Identifier: MIT OR Apache-2.0
//! Picasso APCB v2 SPD/GPIO editor.
//! No AGESA structure synthesis: edit the supplied board-appropriate template.
//! Channel 1 DIMM 1 uses its own SPD, not DIMM 0's.
use super::Result;
use amd_apcb::memory::SpdDescriptor;
use zerocopy::{FromBytes, IntoBytes};
const GPIO_MAGIC: [u8; 12] = [
    0xfa, 0xde, 0xdd, 0xad, 0xfa, 0xde, 0xdd, 0xad, 0xfa, 0xde, 0xdd, 0xad,
];
const SPD_MAGIC: [u8; 6] = [0xf0, 0x05, 0xba, 0x11, 0, 0];

/// Four (GPIO number, IOMUX selector, bank control) triples in RAM-ID bit order.
pub fn inject(
    template: &[u8],
    gpio: [[u8; 3]; 4],
    spds: [[Option<&[u8; 512]>; 2]; 2],
    strip_manufacturer: bool,
) -> Result<Vec<u8>> {
    if template.len() < 17 || !template.starts_with(b"APCB") {
        return Err("not an APCB template".into());
    }
    let gpio_offsets: Vec<_> = template
        .windows(12)
        .enumerate()
        .filter_map(|(i, b)| (b == GPIO_MAGIC).then_some(i))
        .collect();
    if gpio_offsets.len() != 1 || gpio_offsets[0] == 0 {
        return Err("APCB requires exactly one GPIO sentinel".into());
    }
    let gpio_offset = gpio_offsets[0];
    let mut output = template.to_vec();
    for (i, triple) in gpio.iter().enumerate() {
        output[gpio_offset + i * 3..gpio_offset + i * 3 + 3].copy_from_slice(triple);
    }
    let mut cursor = 0;
    let mut found = 0;
    while let Some(relative) = template[cursor..].windows(6).position(|b| b == SPD_MAGIC) {
        let offset = cursor + relative;
        let descriptor_start = offset
            .checked_sub(28)
            .ok_or("truncated APCB SPD descriptor")?;
        let end = offset
            .checked_add(512)
            .filter(|&end| end <= template.len())
            .ok_or("truncated APCB SPD slot")?;
        if descriptor_start < gpio_offset + 12 && end > gpio_offset {
            return Err("APCB GPIO/SPD sentinels overlap".into());
        }
        let mut descriptor = SpdDescriptor::read_from_bytes(&template[descriptor_start..offset])
            .map_err(|_| "invalid APCB SPD descriptor")?;
        let channel = descriptor.channel().map_err(|e| format!("{e:?}"))?;
        let dimm = descriptor.dimm().map_err(|e| format!("{e:?}"))?;
        if channel > 1 || dimm > 1 {
            return Err("APCB DIMM/channel selector exceeds one".into());
        }
        let spd = spds[usize::from(channel)][usize::from(dimm)];
        descriptor.set_dram_down_spd_valid(spd.is_some());
        descriptor.set_dimm_present(spd.is_some());
        output[descriptor_start..offset].copy_from_slice(descriptor.as_bytes());
        output[offset..end].copy_from_slice(spd.unwrap_or(&[0; 512]));
        if strip_manufacturer {
            output[offset + 320..offset + 384].fill(0);
        }
        cursor = end;
        found += 1;
    }
    if found == 0 {
        return Err("APCB contains no SPD slots".into());
    }
    output[16] = 0;
    output[16] = output
        .iter()
        .fold(0u8, |sum, &b| sum.wrapping_add(b))
        .wrapping_neg();
    Ok(output)
}

pub use fstart_platform_amd_apcb::parse_spd_hex;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn inject_dimm1_and_checksum() {
        let mut template = vec![0; 128 + 28 + 512];
        template[..4].copy_from_slice(b"APCB");
        template[32..44].copy_from_slice(&GPIO_MAGIC);
        template[128 + 25] = 1;
        template[128 + 26] = 1;
        template[156..162].copy_from_slice(&SPD_MAGIC);
        let spd0 = [0xaa; 512];
        let spd1 = [0xbb; 512];
        let output = inject(
            &template,
            [[132, 2, 0], [90, 2, 0], [86, 1, 0], [69, 0, 0]],
            [[None, None], [Some(&spd0), Some(&spd1)]],
            true,
        )
        .unwrap();
        assert_eq!(output.len(), template.len());
        assert_eq!(output[156], 0xbb);
        assert_eq!(output[128], 1);
        assert_eq!(&output[156 + 320..156 + 384], &[0; 64]);
        assert_eq!(output.iter().fold(0u8, |s, &b| s.wrapping_add(b)), 0);
        assert!(inject(&template[..160], [[0; 3]; 4], [[None; 2]; 2], false).is_err());
    }
}
