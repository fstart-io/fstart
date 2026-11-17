use super::*;

#[test]
fn vscc_writes_only_the_unaligned_low_word_when_requested() {
    let mut registers = [0xa5u8; 0x4000];
    for value in [None, Some(0x2005), Some(0x2009)] {
        registers.fill(0xa5);
        IntelIch8::configure_spi_vscc(Rcba::new(registers.as_mut_ptr() as usize), value);
        let mut expected = [0xa5; 0x4000];
        if let Some(value) = value {
            let offset = ICH8_SPIBAR_OFFSET + SPIBAR_VSCC;
            expected[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
        }
        assert_eq!(registers, expected);
    }
}
