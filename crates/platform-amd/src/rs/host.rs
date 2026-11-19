// SPDX-License-Identifier: MIT OR Apache-2.0
//! The host build of an RS board: one image with the PSP firmware of its
//! SoC packed from the blob tables (`--blobs` or `--donor`), the FFS
//! between the PSP partition and the reset image, which ends the 16 MiB the
//! boot ROM maps ([`crate::host::single`]).
extern crate std;
use super::Variant;
use crate::host::{Host, single};
use fstart_amdfw_format::efs::BiosDirectorySlot;

/// The host data of a board with the RS SoC `V` and up to `max_cpus` CPUs.
pub fn host<V: Variant>(max_cpus: u16) -> Host {
    let layout = V::LAYOUT;
    Host {
        name: V::NAME,
        layout,
        max_cpus,
        tseg_size: super::TSEG_SIZE.into(),
        release_only: true,
        i2c_input: false,
        image: crate::host::Image::Single(single::Image {
            soc: V::NAME,
            bios_directory: BiosDirectorySlot::Family19h,
            apob_destination: V::APOB.base,
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
    use crate::host::Image;
    use fstart_image_build::options::OptionStore;

    #[test]
    fn the_reset_image_ends_the_boot_window_after_ffs_and_nvram() {
        for host in [
            host::<crate::genoa::Stones>(192),
            host::<crate::siena::Dense>(128),
        ] {
            let layout = host.layout;
            let ffs = layout.ffs.unwrap();
            let Image::Single(image) = host.image else {
                panic!("one image")
            };
            let Some(reset) = image.source.reset_offset else {
                panic!("a whole image")
            };
            assert_eq!(u64::from(reset) + layout.image.size, layout.flash.size);
            match host.option_store {
                Some(OptionStore::Varstore { offset, size }) => {
                    assert_eq!((offset, size), (0xcc_0000, 0x4_0000));
                    assert_eq!(ffs.end(), layout.flash.base + u64::from(offset));
                    assert_eq!(offset + size, reset);
                }
                None => assert_eq!(ffs.end(), layout.flash.base + u64::from(reset)),
                other => panic!("unexpected settings store: {other:?}"),
            }
        }
    }
}
