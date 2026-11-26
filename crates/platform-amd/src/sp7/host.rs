// SPDX-License-Identifier: MIT OR Apache-2.0
//! Host build plan for SP7 boards: a 64 MiB Venice ISH A/B dual-origin flash
//! image packed from the SoC's and board's blob tables (`--blobs` or
//! `--donor`), with the FFS and x86 reset image in Bank 0 (ROM2 window
//! `0xFF00_0000..0xFFFF_FFFF`, `bios_origin = 0`) and EFS -> `$PSP` L1 ->
//! ISH A/B (`0x48`/`0x4A`) -> `$PL2` -> `0x49` -> `$BL2` in Bank 1
//! (`image_origin = 0x0100_0000`).

use super::Variant;
use crate::host::{Host, Image, single};
use fstart_amdfw_format::efs::BiosDirectorySlot;

/// The host build plan of an SP7 board carrying SoC `V` and up to `max_cpus`
/// logical threads.
pub fn host<V: Variant>(max_cpus: u16) -> Host {
    let layout = V::LAYOUT;
    Host {
        name: V::NAME,
        layout,
        max_cpus,
        tseg_size: super::TSEG_SIZE.into(),
        release_only: true,
        i2c_input: false,
        image: Image::Single(single::Image {
            soc: V::NAME,
            bios_directory: BiosDirectorySlot::Family19h,
            apob_destination: V::APOB_BASE,
            flash_size: Some(V::ROM_SIZE),
            source: single::Source {
                reset_offset: Some((layout.flash.size - layout.image.size) as u32),
                psp: Default::default(),
            },
        }),
        option_store: V::NVRAM.map(
            |region| fstart_image_build::options::OptionStore::Varstore {
                offset: (region.base - layout.flash.base) as u32,
                size: region.size as u32,
            },
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::HostOptions;
    use fstart_image_build::plan::BuildSelection;

    #[test]
    fn venice_host_plan_places_reset_at_end_of_bank0_and_packs_64m_ish() {
        let h = host::<crate::venice::Wh>(512);
        let layout = h.layout;
        let Image::Single(image) = &h.image else {
            panic!("single image");
        };
        assert_eq!(image.soc, "venice");
        assert_eq!(image.flash_size, Some(64 << 20));
        assert_eq!(image.source.reset_offset, Some(0x00d0_0000));
        assert_eq!(layout.image.base, 0x7000_0000);
        assert_eq!(crate::venice::Wh::APOB_BASE, 0x7700_0000);
        assert!(!layout.early.overlaps(&crate::layout::Region::new(
            crate::venice::Wh::APOB_BASE,
            crate::venice::Wh::APOB_MAX_SIZE,
        )));
        assert_eq!(h.tseg_size, 16 << 20);

        let selection = BuildSelection::default();
        let ok = crate::host::resolve(&h, &selection, &HostOptions::default()).unwrap();
        assert_eq!(ok.images.len(), 1); // FFS only without blobs
    }
}
