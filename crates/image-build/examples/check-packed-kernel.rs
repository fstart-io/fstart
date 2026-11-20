//! Independent readback of the supplied ARMv7 kernel in CI's parity image.
use fstart_core::ffs::{Compression, EntryContent, FileType};
use fstart_ffs::{FfsReader, lz4};
use std::{env, fs};

fn main() {
    let mut args = env::args_os().skip(1);
    let image = fs::read(args.next().expect("FFS path")).unwrap();
    let expected = fs::read(args.next().expect("kernel fixture path")).unwrap();
    let reader = FfsReader::new(&image);
    let anchor = reader
        .read_anchor(reader.scan_for_anchor().unwrap())
        .unwrap();
    let manifest = reader.read_manifest(&anchor).unwrap();
    let region = FfsReader::find_region(&manifest, "ro").unwrap();
    let entry = FfsReader::find_entry(region, "zImage").unwrap();
    let EntryContent::File {
        file_type: FileType::Payload,
        segments,
        ..
    } = &entry.content
    else {
        panic!("zImage is not a payload");
    };
    let [segment] = segments.as_slice() else {
        panic!("expected one flat kernel segment");
    };
    assert_eq!(segment.loaded_size as usize, expected.len());
    let stored = reader.read_segment_data(segment, region, entry).unwrap();
    match segment.compression {
        Compression::None => assert_eq!(stored, expected),
        Compression::Lz4 => {
            let mut bytes = vec![0; expected.len()];
            assert_eq!(
                lz4::decompress_block(stored, &mut bytes).unwrap(),
                expected.len()
            );
            assert_eq!(bytes, expected);
        }
        other => panic!("unexpected kernel compression: {other:?}"),
    }
}
