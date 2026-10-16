use super::*;
use crate::x86::mtrr::MsrEntry;
use zerocopy::FromBytes;

#[test]
fn linked_sipi_page_holds_exact_msr_records_without_changing_neighbors() {
    let entries = [
        MsrEntry {
            index: 0x200,
            low: 0x12345006,
            high: 1,
        },
        MsrEntry {
            index: 0x2ff,
            low: 0xc00,
            high: 0,
        },
    ];
    let mut page = [0x55; 4096];
    assert_eq!(sipi_blob::TRAMPOLINE.len(), page.len());
    assert!(sipi_blob::MSR_COUNT_OFFSET + 4 <= sipi_blob::MSR_TABLE_OFFSET);
    patch_msr_table(&mut page, &entries).unwrap();
    assert_eq!(
        u32::from_le_bytes(
            page[sipi_blob::MSR_COUNT_OFFSET..sipi_blob::MSR_COUNT_OFFSET + 4]
                .try_into()
                .unwrap()
        ),
        2
    );
    for (bytes, expected) in page[sipi_blob::MSR_TABLE_OFFSET..sipi_blob::MSR_TABLE_OFFSET + 24]
        .chunks_exact(12)
        .zip(entries)
    {
        let actual = MsrEntry::read_from_bytes(bytes).unwrap();
        assert_eq!(actual.index, expected.index);
        assert_eq!(actual.low, expected.low);
        assert_eq!(actual.high, expected.high);
    }
    assert!(
        page[..sipi_blob::MSR_COUNT_OFFSET]
            .iter()
            .all(|byte| *byte == 0x55)
    );
    assert!(
        page[sipi_blob::MSR_TABLE_OFFSET + 24..]
            .iter()
            .all(|byte| *byte == 0x55)
    );

    let mut short = [0x55; 4096];
    let end = sipi_blob::MSR_TABLE_OFFSET + 23;
    assert!(patch_msr_table(&mut short[..end], &entries).is_err());
    assert!(short.iter().all(|byte| *byte == 0x55));
}
