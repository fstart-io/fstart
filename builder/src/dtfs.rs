/*++

Licensed under the Apache-2.0 license.

File Name:

lib.rs

Abstract:

File contains exports for the DTFS builder

--*/

#![allow(dead_code)]
#![allow(unused_imports)]

use core::fmt;
use core::mem::size_of;
use devicetree_tool::DeviceTree;
use fdt::Fdt;
use std::cmp::Ordering;
use std::default;
use std::fs;
use std::process::Command;
use vm_fdt::FdtWriter;
use zerocopy::{AsBytes, FromBytes, FromZeroes};

use fstart_fs::metadata::*;

fn dtb_from_dts(dts_path: &str) -> Vec<u8> {
    let dts = std::fs::read_to_string(dts_path).expect("Unable to read input file");
    // This panics on wrong input
    let tree = DeviceTree::from_dts_bytes(dts.as_bytes());
    tree.generate_dtb()
}

#[derive(PartialEq, PartialOrd, Eq, Debug, Clone)]
struct DtfsFlashinfo {
    board_name: String,
    category: BoardCategory,
    board_url: String,
    memory_mapping: Option<Vec<MemoryMap>>,
    medium_type: MediumType,
    medium_size: u32,
}

#[derive(Debug, PartialEq, Eq, Clone)]
struct DtfsDigest {
    algo: HashAlgo,
    digest: Vec<u8>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct DtfsArea {
    description: String,
    compatible: String,
    offset: FlashAddress,
    area_size: u32,
    file: Option<Vec<u8>>,
    mem_size: Option<u32>,
    digests: Option<Vec<DtfsDigest>>,
    compression_type: Option<CompressionAlgo>,
}

impl Ord for DtfsArea {
    fn cmp(&self, other: &Self) -> Ordering {
        self.offset.0.cmp(&other.offset.0)
    }
}

impl PartialOrd for DtfsArea {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Clone, Debug)]
struct Dtfs {
    flashinfo: DtfsFlashinfo,
    areas: Vec<DtfsArea>,
}

#[derive(Debug)]
enum DtfsError {
    Missing,
    OverlappingAreas,
    VmFdtError(vm_fdt::Error),
}

impl Dtfs {
    // move this in the generate function and map the vm_fdt output
    fn validate_dtfs(&mut self) -> Result<(), DtfsError> {
        self.areas.sort();

        // Check for no overlap
        for i in 1..self.areas.len() {
            let prev_start = self.areas[i - 1].offset.0;
            let prev_end = prev_start + self.areas[i - 1].area_size - 1;
            let cur_start = self.areas[i].offset.0;

            if prev_end > cur_start {
                return Err(DtfsError::OverlappingAreas);
            }
        }
        Ok(())
    }

    fn generate_fdt(&mut self) -> Result<Vec<u8>, vm_fdt::Error> {
        let mut fdt = FdtWriter::new()?;

        let root = fdt.begin_node("")?;
        fdt.property_u32("#address-cells", 0x2)?;
        fdt.property_u32("#size-cells", 0x2)?;

        let flash_info = fdt.begin_node("flash-info")?;
        fdt.property_string("compatible", "fstart-flashinfo")?;
        fdt.property_string("board-name", &self.flashinfo.board_name)?;
        fdt.property_string("board-category", self.flashinfo.category.name())?;
        fdt.property_string("board-url", &self.flashinfo.board_url)?;
        fdt.property_string("medium-type", self.flashinfo.medium_type.name())?;
        if let Some(mmaps) = &self.flashinfo.memory_mapping {
            let memory_maps: Vec<u64> = mmaps.iter().fold(Vec::new(), |mut acc, map| {
                acc.push(map.flash_address.0.into());
                acc.push(map.mapped_address.0);
                acc.push(map.size.into());
                acc
            });
            fdt.property_array_u64("memory-map", &memory_maps)?;
        }
        self.areas.sort();
        for (n, area) in self.areas.iter().enumerate() {
            let name = format!("area{}", n);
            let fdt_area = fdt.begin_node(&name)?;
            fdt.property_string("description", &area.description)?;
            fdt.property_string("compatible", &area.compatible)?;
            fdt.property_u64("offset", area.offset.0.into())?;
            fdt.property_u64("area-size", area.area_size.into())?;
            if let Some(mem_size) = area.mem_size {
                fdt.property_u64("mem-size", mem_size.into())?;
            }
            if let Some(digests) = &area.digests {
                for digest in digests {
                    fdt.property(digest.algo.name(), &digest.digest)?;
                }
            }
            if let Some(compression) = &area.compression_type {
                fdt.property_string("compression", compression.name())?;
            }
            fdt.end_node(fdt_area)?;
        }

        fdt.end_node(flash_info)?;
        fdt.end_node(root)?;
        let fdt = fdt.finish()?;
        Ok(fdt)
    }

    fn generate_fdt_area(&mut self) -> Result<(), DtfsError> {
        let fdt_bin = self.generate_fdt().map_err(DtfsError::VmFdtError)?;

        const SINGATURE_ALIGNMENT: usize = 16;
        let signatures_offset = (size_of::<DtfsHeader>() + fdt_bin.len() + SINGATURE_ALIGNMENT - 1)
            & !(SINGATURE_ALIGNMENT - 1);
        let signatures_offset = signatures_offset.try_into().unwrap();

        // TODO Add signatures & struct pointing to signatures & MAGIC
        let header = DtfsHeader::new(signatures_offset);

        let dtfs_area = self
            .areas
            .iter_mut()
            .find(|x| x.compatible == "fstart-primary-dtfs")
            .unwrap();

        let mut bin = vec![0xff; dtfs_area.area_size.try_into().unwrap()];
        bin[..size_of::<DtfsHeader>()].copy_from_slice(header.as_bytes());
        bin[size_of::<DtfsHeader>()..][..fdt_bin.len()].copy_from_slice(&fdt_bin);

        dtfs_area.file = Some(bin);

        Ok(())
    }

    fn generate_bin(&self) -> Vec<u8> {
        let mut bin = vec![0xff; self.flashinfo.medium_size.try_into().unwrap()];
        for area in &self.areas {
            if let Some(file) = &area.file {
                if file.len() > area.area_size.try_into().unwrap() {
                    panic!(
                        "File ({}) for area {} is larger than area size",
                        file.len(),
                        area.description
                    );
                }

                bin[area.offset.0.try_into().unwrap()..][..file.len()].copy_from_slice(file);
            }
        }
        bin
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn build_image_with_1_raw_bin() {
        let dts_path = concat!(env!("CARGO_MANIFEST_DIR"), "/test-data/raw_bin_test.dts");

        let dtb = dtb_from_dts(dts_path);
        let _parsed_fdt = fdt::Fdt::new(dtb.as_slice()).unwrap();
    }

    #[test]
    fn test_generate_test_dtfs() {
        const DTFS_BASE: u32 = 0x1000;

        let mut dtfs = Dtfs {
            flashinfo: DtfsFlashinfo {
                board_name: String::from("test"),
                category: BoardCategory::Other,
                board_url: String::from("https://fstart.io/"),
                memory_mapping: Some(vec![MemoryMap {
                    flash_address: FlashAddress(0),
                    mapped_address: MappedAddress(0x1000),
                    size: 0x1_0000,
                }]),
                medium_type: MediumType::Other,
                medium_size: 0x1_0000,
            },
            areas: vec![
                DtfsArea {
                    description: String::from("DTFS"),
                    compatible: String::from("fstart-primary-dtfs"),
                    offset: FlashAddress(DTFS_BASE),
                    area_size: 0x5000,
                    ..Default::default()
                },
                DtfsArea {
                    description: String::from("ZEROS"),
                    compatible: String::from("fstart-raw-bin"),
                    offset: FlashAddress(0),
                    area_size: 0x1000,
                    file: Some(vec![0u8; 0x1000]),
                    ..Default::default()
                },
            ],
        };
        dtfs.validate_dtfs().unwrap();
        dtfs.generate_fdt_area().unwrap();
        let bin = dtfs.generate_bin();

        // Look for magic
        assert_eq!(bin[DTFS_BASE as usize..][..0x10], *DtfsHeader::DTFS_MAGIC);
        // Make sure the FDT is valid
        let _fdt = Fdt::new(&bin[0x1020..]).unwrap();
    }
}
