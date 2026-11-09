use super::*;
use crate::x86::mtrr::MsrEntry;
use zerocopy::FromBytes;

#[test]
fn sipi_post_replay_patches_own_cpu_routing_without_a_bsp_syscfg_copy() {
    use crate::x86::cpu::amd::msr::{SYSCFG, Syscfg};
    let legacy = SipiUnlock::new(SYSCFG, Syscfg::MTRR_FIX_DRAM_MOD_EN::SET);
    assert_eq!(
        (legacy.post_set, legacy.post_test, legacy.post_then),
        (0, 0, 0)
    );
    let rs = legacy.with_post_replay(
        Syscfg::MTRR_FIX_DRAM_EN::SET + Syscfg::MTRR_VAR_DRAM_EN::SET,
        Syscfg::TOM2_EN::SET,
        Syscfg::TOM2_WB::SET,
    );
    let mut page = [0x55; 4096];
    patch_sipi_unlock(&mut page, Some(rs)).unwrap();
    let get = |offset| u32::from_le_bytes(page[offset..offset + 4].try_into().unwrap());
    assert_eq!(get(sipi_blob::UNLOCK_MSR_OFFSET), 0xc001_0010);
    assert_eq!(get(sipi_blob::UNLOCK_BITS_OFFSET), 1 << 19);
    assert_eq!(get(sipi_blob::POST_SET_OFFSET), 0x140000);
    assert_eq!(get(sipi_blob::POST_TEST_OFFSET), 1 << 21);
    assert_eq!(get(sipi_blob::POST_THEN_OFFSET), 1 << 22);
    assert!(
        page[..sipi_blob::UNLOCK_MSR_OFFSET]
            .iter()
            .all(|&v| v == 0x55)
    );
    assert!(
        page[sipi_blob::POST_THEN_OFFSET + 4..]
            .iter()
            .all(|&v| v == 0x55)
    );
    let mut short = [0x55; 4096];
    assert!(patch_sipi_unlock(&mut short[..sipi_blob::POST_THEN_OFFSET + 3], Some(rs)).is_err());
    assert!(short.iter().all(|&v| v == 0x55));
    patch_sipi_unlock(&mut page, None).unwrap();
    assert!(
        page[sipi_blob::UNLOCK_MSR_OFFSET..sipi_blob::POST_THEN_OFFSET + 4]
            .iter()
            .all(|&v| v == 0)
    );
}

#[test]
fn failed_physical_copy_is_rejected_before_the_platform_launcher_runs() {
    let expected = sipi_blob::TRAMPOLINE;
    let mut actual = expected.to_vec();
    assert!(unsafe { verify_sipi_page(actual.as_ptr(), expected) }.is_ok());
    actual.fill(0);
    assert!(matches!(
        unsafe { verify_sipi_page(actual.as_ptr(), expected) },
        Err(MpError::TrampolinePlacementFailed)
    ));
    actual.copy_from_slice(expected);
    actual[sipi_blob::MSR_TABLE_OFFSET + 16] ^= 0xff;
    assert!(matches!(
        unsafe { verify_sipi_page(actual.as_ptr(), expected) },
        Err(MpError::TrampolinePlacementFailed)
    ));
}

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
