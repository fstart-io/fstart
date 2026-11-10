//! Independent wire-layout checks for the YAML migration, not generated tests.

use super::registers::*;
use core::mem::{offset_of, size_of};
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};

#[test]
fn rcba_layout_retains_mixed_widths_and_spi_arrays() {
    assert_eq!(offset_of!(RcbaRegs, v0ctl), 0x14);
    assert_eq!(offset_of!(RcbaRegs, lctl), 0x1a8);
    assert_eq!(offset_of!(RcbaRegs, cir_2027), 0x2027);
    assert_eq!(offset_of!(RcbaRegs, spi), 0x3020);
    assert_eq!(offset_of!(RcbaRegs, spi_ctrl), 0x3022);
    assert_eq!(offset_of!(RcbaRegs, spi_data), 0x3028);
    assert_eq!(offset_of!(RcbaRegs, spi_preop), 0x3074);
    assert_eq!(offset_of!(RcbaRegs, spi_optype), 0x3076);
    assert_eq!(offset_of!(RcbaRegs, spi_opmenu), 0x3078);
    assert_eq!(offset_of!(RcbaRegs, oic), 0x31ff);
    assert_eq!(offset_of!(RcbaRegs, gcs), 0x3410);
    assert_eq!(offset_of!(RcbaRegs, port_3e4e), 0x3e4e);
    assert_eq!(size_of::<RcbaRegs>(), 0x3e50);
    assert_eq!(rcba::SPI_DATA_COUNT, 64);
    assert_eq!(rcba::SPI_DATA_STRIDE, 1);
    assert_eq!(rcba::SPI_PREOP_COUNT, 2);
    assert_eq!(rcba::SPI_OPMENU_COUNT, 8);
}

#[test]
fn lpc_overlay_keeps_the_shared_header_and_existing_offsets() {
    assert_eq!(offset_of!(Ich7LpcPciConfig, command), 0x04);
    assert_eq!(offset_of!(Ich7LpcPciConfig, bar), 0x10);
    assert_eq!(offset_of!(Ich7LpcPciConfig, pmbase), 0x40);
    assert_eq!(offset_of!(Ich7LpcPciConfig, serirq_cntl), 0x64);
    assert_eq!(offset_of!(Ich7LpcPciConfig, gen_dec), 0x84);
    assert_eq!(offset_of!(Ich7LpcPciConfig, gen_pmcon_1), 0xa0);
    assert_eq!(offset_of!(Ich7LpcPciConfig, gen_pmcon_3), 0xa4);
    assert_eq!(offset_of!(Ich7LpcPciConfig, etr3), 0xac);
    assert_eq!(offset_of!(Ich7LpcPciConfig, rcba_reg), 0xf0);
    assert_eq!(size_of::<Ich7LpcPciConfig>(), 0x1000);
    assert_eq!(lpc::GEN_DEC_COUNT, 4);
    assert_eq!(lpc::GEN_DEC_STRIDE, 4);
    assert_eq!(super::ich7::RCBA_REG, 0xf0);
    assert_eq!(super::ich7::GCS, 0x3410);
}

#[test]
fn field_encodings_match_the_original_driver() {
    assert_eq!(GCS_REG::NO_REBOOT::SET.value, 1 << 5);
    assert_eq!(HPTC_REG::ENABLE::SET.value, 1 << 7);
    assert_eq!(FD_REG::RAW.val(u32::MAX).mask(), u32::MAX);
    assert_eq!(GEN_PMCON_1_REG::SMI_LOCK::SET.value, 1 << 4);
    assert_eq!(GEN_PMCON_3_REG::SLP_S3_STRETCH.val(3).value, 0x30);
    assert_eq!(ETR3_REG::CF9LOCK::SET.value, 1 << 31);
    assert_eq!(SERIRQ_REG::MODE::Continuous.value, 0x40);
    assert_eq!(SERIRQ_REG::START_PULSE::Clocks8.value, 2);
    assert_eq!(SPI_REG::CYCLE_DONE::SET.value, 4);
    assert_eq!(SPI_REG::CYCLE_ERROR::SET.value, 8);
    assert_eq!(SPI_CTRL_REG::OPCODE_INDEX.val(7).mask(), 0x70);
    assert_eq!(SPI_CTRL_REG::DATA_BYTES.val(63).mask(), 0x3f00);
}

#[test]
fn generated_registers_preserve_neighboring_bytes_and_unrelated_bits() {
    // Backing storage is aligned and covers the entire RCBA window. No actual
    // device is accessed; this exercises the existing barrier-aware transport.
    let mut storage = [0u32; 0x1000];
    // SAFETY: initialized, aligned storage; the overlay contains UnsafeCells
    // and is the only view used while the reference exists.
    let regs = unsafe { &*(storage.as_mut_ptr().cast::<RcbaRegs>()) };
    regs.spi.set(0xa5a5);
    regs.spi_ctrl.set(0x8000);
    regs.spi_ctrl
        .modify(SPI_CTRL_REG::OPCODE_INDEX.val(5) + SPI_CTRL_REG::DATA_BYTES.val(15));
    assert_eq!(regs.spi.get(), 0xa5a5);
    assert_eq!(regs.spi_ctrl.get(), 0x8f50);
    regs.spi_data[63].set(0x5a);
    regs.spi_preop[1].set(0x50);
    assert_eq!(regs.spi_data[62].get(), 0);
    assert_eq!(regs.spi_data[63].get(), 0x5a);
    assert_eq!(regs.spi_preop[0].get(), 0);
    assert_eq!(regs.spi_preop[1].get(), 0x50);
}
