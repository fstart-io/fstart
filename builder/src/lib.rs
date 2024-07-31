/*++

Licensed under the Apache-2.0 license.

File Name:

lib.rs

Abstract:

File contains exports for fstart Library.

--*/

#![allow(dead_code)]
#![allow(unused_imports)]

use core::fmt;
use fdt::Fdt;
use std::default;
use std::fs::File;
use std::io::Error;
use std::io::Write;
use std::process::Command;
use vm_fdt::FdtWriter;

fn dtb_from_dts(dts_path: &str) -> Result<Vec<u8>, Error> {
    let output = Command::new("dtc")
        .args(["-I", "dts"])
        .arg(dts_path)
        .args(["-O", "dtb"])
        .arg("-Wno-unit_address_vs_reg")
        .output()?;

    if !output.status.success() {
        let msg = format!("dtc failed: {:?}", String::from_utf8(output.stderr));
        return Err(Error::new(std::io::ErrorKind::InvalidInput, msg));
    }
    Ok(output.stdout)
}

#[test]
fn build_image_with_1_raw_bin() {
    let dts_path = concat!(env!("CARGO_MANIFEST_DIR"), "/test-data/raw_bin_test.dts");

    let dtb = dtb_from_dts(dts_path).unwrap();
    let _parsed_fdt = fdt::Fdt::new(dtb.as_slice()).unwrap();
}

#[test]
fn build_image_with_1_raw_bin_fail() {
    let dts_path = concat!(env!("CARGO_MANIFEST_DIR"), "/FILE_DOES_NOT_EXIST");

    let result = dtb_from_dts(dts_path);
    assert!(result.is_err());
}

#[derive(PartialEq, PartialOrd, Default)]
struct FlashAddress(u32);
#[derive(PartialEq, PartialOrd, Default)]
struct MappedAddress(u64);

struct MemoryMap {
    flash_address: FlashAddress,
    mapped_address: MappedAddress,
    size: u32,
}

impl MemoryMap {
    fn is_mapped(&self, base: MappedAddress, size: u32) -> bool {
        let begin = base.0;
        let end = base.0 + u64::from(size);

        if begin < self.mapped_address.0 {
            return false;
        }
        if end > self.mapped_address.0 + u64::from(self.size) {
            return false;
        }
        true
    }
}

enum BoardCategory {
    Client,
    Embedded,
    Server,
    Other,
}

impl BoardCategory {
    fn name(&self) -> &'static str {
        match self {
            Self::Client => "client",
            Self::Embedded => "embedded",
            Self::Server => "server",
            Self::Other => "other",
        }
    }
}

enum MediumType {
    SpiFlash,
    Mmc,
    Other,
}

impl MediumType {
    fn name(&self) -> &'static str {
        match self {
            Self::SpiFlash => "spi-flash",
            Self::Mmc => "mmc",
            Self::Other => "other",
        }
    }
}

struct DtfsFlashinfo {
    board_name: String,
    category: BoardCategory,
    board_url: String,
    memory_mapping: Option<Vec<MemoryMap>>,
    medium_type: MediumType,
    medium_size: u32,
}

enum HashAlgo {
    //    Sha256,
    //    Sha384,
    Sha512,
    // TODO are these good targets??
    //    SlhDsaShake128s,
    //    SlhDsaShake196s,
    SlhDsaShake256s,
}

impl HashAlgo {
    fn name(&self) -> &'static str {
        match self {
            Self::Sha512 => "sha512",
            Self::SlhDsaShake256s => "slh_dsa_shake_256s",
        }
    }
}

struct DtfsDigest {
    algo: HashAlgo,
    digest: Vec<u8>,
}

enum CompressionAlgo {
    Lz4,
    Lzma,
    Zstd,
}

impl CompressionAlgo {
    fn name(&self) -> &'static str {
        match self {
            Self::Lz4 => "lz4",
            Self::Lzma => "lzma",
            Self::Zstd => "zstd",
        }
    }
}

#[derive(Default)]
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

struct Dtfs {
    flashinfo: DtfsFlashinfo,
    areas: Vec<DtfsArea>,
}

impl Dtfs {
    fn generate_fdt(&self) -> Result<Vec<u8>, vm_fdt::Error> {
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

        let fdt = fdt.finish().unwrap();

        Ok(fdt)
    }
}

#[test]
fn test_generate_test_dtfs() {
    let dtfs = Dtfs {
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
        areas: vec![DtfsArea {
            description: String::from("DTFS"),
            compatible: String::from("fstart-primary-dtfs"),
            offset: FlashAddress(0x1000),
            area_size: 0x5000,
            ..Default::default()
        }],
    };
    dtfs.generate_fdt().unwrap();
}
