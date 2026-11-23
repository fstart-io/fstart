// SPDX-License-Identifier: MIT OR Apache-2.0
//! Host build plan for SP7 boards.
//!
//! Venice boots through EFS -> `$PSP` L1 -> 32-byte ISH A/B slots (`0x48`/`0x4A`)
//! -> `$PL2` -> `0x49` -> `$BL2` with dual-origin mode-1 BIOS reset images.
//! Until an ISH A/B flash packer exists, this host plan emits build-only
//! stage + SMM + FFS artifacts and explicitly rejects `--blobs`
//! flash assembly rather than emitting an unbootable legacy image.

use super::Variant;
use crate::host::{Host, Image};

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
        image: Image::BuildOnly {
            reason: "flash image packing is not yet implemented (Venice requires ISH A/B dual-origin assembly)",
        },
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
    fn venice_host_plan_is_build_only_and_rejects_flash_packing() {
        let h = host::<crate::venice::Wh>(512);
        let layout = h.layout;
        assert!(matches!(h.image, Image::BuildOnly { .. }));
        assert_eq!(layout.image.base, 0x7000_0000);
        assert_eq!(crate::venice::Wh::APOB_BASE, 0x7700_0000);
        assert!(!layout.early.overlaps(&crate::layout::Region::new(
            crate::venice::Wh::APOB_BASE,
            crate::venice::Wh::APOB_MAX_SIZE,
        )));
        assert_eq!(h.tseg_size, 16 << 20);

        let selection = BuildSelection::default();
        let ok = crate::host::resolve(&h, &selection, &HostOptions::default()).unwrap();
        assert_eq!(ok.images.len(), 1); // FFS only

        let with_blobs = HostOptions {
            blobs: Some("blobs".into()),
            ..HostOptions::default()
        };
        assert!(crate::host::resolve(&h, &selection, &with_blobs).is_err());
    }
}
