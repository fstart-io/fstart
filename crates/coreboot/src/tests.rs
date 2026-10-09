use super::*;

fn records(table: &[u8]) -> impl Iterator<Item = (u32, &[u8])> {
    let mut rest = &table[HEADER_SIZE..];
    core::iter::from_fn(move || {
        let (header, _) = RecordHeader::ref_from_prefix(rest).ok()?;
        let (record, tail) = rest.split_at(header.size.get() as usize);
        rest = tail;
        Some((header.tag.get(), &record[RECORD_HEADER_SIZE..]))
    })
}

#[test]
fn table_checksums_verify_like_payloads_do() {
    let mut buf = [0u8; 512];
    let mut writer = TableWriter::new(&mut buf).unwrap();
    writer.mainboard("Foxconn", "D41S").unwrap();
    writer.acpi_rsdp(0x7fe0_0000).unwrap();
    writer
        .file("vgaroms/seavgabios.bin", 0x5000_0000, 1234)
        .unwrap();
    let table = writer.finish();

    let (header, records_bytes) = Header::ref_from_prefix(table).unwrap();
    assert_eq!(header.signature, *b"LBIO");
    // SeaBIOS: the header sums to zero, the records to table_checksum.
    assert_eq!(checksum(&table[..HEADER_SIZE]), 0);
    assert_eq!(
        u32::from(checksum(records_bytes)),
        header.table_checksum.get()
    );
    assert_eq!(header.table_entries.get(), 3);
    assert_eq!(records_bytes.len() as u32, header.table_bytes.get());

    let found: heapless::Vec<_, 4> = records(table).collect();
    assert_eq!(found[0].0, tag::MAINBOARD);
    assert_eq!(&found[0].1[..15], b"\0\x08Foxconn\0D41S\0");
    assert_eq!(found[2].0, tag::FSTART_FILE);
    assert_eq!(&found[2].1[16..39], b"vgaroms/seavgabios.bin\0");
    assert!(found.iter().all(|(_, body)| (body.len() + 8) % 4 == 0));
}

#[test]
fn memory_map_marks_table_overlaps() {
    let mut buf = [0u8; 512];
    let mut writer = TableWriter::new(&mut buf).unwrap();
    writer
        .memory(
            [
                (0, 0x1000, mem::RESERVED),
                (0x1000, 0x9_f000, mem::RAM),
                (0x10_0000, 0x7ff0_0000, mem::RAM),
            ],
            &[(0x5000_0000, 0x1_0000), (0, 0x1000)],
        )
        .unwrap();
    let table = writer.finish();
    let (_, body) = records(table).next().unwrap();
    let ranges = <[MemoryRange]>::ref_from_bytes(body).unwrap();
    assert_eq!(
        ranges,
        [
            MemoryRange::new(0, 0x1000, mem::TABLE),
            MemoryRange::new(0x1000, 0x9_f000, mem::RAM),
            MemoryRange::new(0x10_0000, 0x4ff0_0000, mem::RAM),
            MemoryRange::new(0x5000_0000, 0x1_0000, mem::TABLE),
            MemoryRange::new(0x5001_0000, 0x2fff_0000, mem::RAM),
        ]
    );
}

#[test]
fn manifest_round_trips_and_sizes_its_window() {
    let vga = [0x55u8; 33_000];
    let files: [(&str, &[u8]); 2] = [
        ("vgaroms/seavgabios.bin", &vga),
        ("etc/sercon-port", &[1; 8]),
    ];
    let mut bytes = [0u8; 34_000];
    let bytes = &mut bytes[..manifest::encoded_len(&files)];
    manifest::encode(0xfd25c, &files, bytes).unwrap();
    let decoded = manifest::Manifest::decode(bytes).unwrap();
    assert_eq!(decoded.entry, 0xfd25c);
    assert!(decoded.files().eq(files));
    assert_eq!(
        decoded.window_size(),
        33_008 + 16 + manifest::TABLE_CAPACITY
    );
    assert!(manifest::Manifest::decode(&bytes[..bytes.len() - 1]).is_none());
}
