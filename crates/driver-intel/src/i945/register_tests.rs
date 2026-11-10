//! Independent expectations carried over from the pre-YAML register catalog.

use super::registers::*;
use core::mem::offset_of;
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};

#[test]
fn hostbridge_offsets_and_access_widths_are_preserved() {
    assert_eq!(offset_of!(I945HostBridgePciConfig, ggc), 0x52);
    assert_eq!(offset_of!(I945HostBridgePciConfig, deven), 0x54);
    assert_eq!(offset_of!(I945HostBridgePciConfig, tom), 0xa0);
    assert_eq!(hostbridge::EPBAR, 0x40);
    assert_eq!(hostbridge::MCHBAR, 0x44);
    assert_eq!(hostbridge::PCIEXBAR, 0x48);
    assert_eq!(hostbridge::GGC, 0x52);
    assert_eq!(peg::VC0RCTL, 0x114);
    assert_eq!(epbar::EPVC1RSTS, 0x26);
    assert_eq!(dmibar::DMILCTL, 0x88);
    assert_eq!(dmibar::DMI_MISC_32, 0x32);
    assert_eq!(mchbar::C0DRT0, 0x110);
    assert_eq!(mchbar::C0DRT0_STRIDE, 0x80);
    assert_eq!(mchbar::C0DRT0_COUNT, 2);
    assert_eq!(super::mchbar::C1_BASE, 0x80);
    assert_eq!(mchbar::DCC, 0x200);
    assert_eq!(mchbar::CPCTL, 0xc16);

    let mut storage = [0u32; 0x40];
    // SAFETY: initialized, aligned storage covers the overlay, exclusively
    // accessed through its UnsafeCell register fields while the view is live.
    let regs = unsafe { &*(storage.as_mut_ptr().cast::<I945HostBridgePciConfig>()) };
    regs.ggc.set(0xff8f);
    regs.deven.set(0xa55a);
    regs.ggc.modify(GGC_REG::GMS.val(4));
    assert_eq!(regs.ggc.get(), 0xffcf);
    assert_eq!(regs.deven.get(), 0xa55a);
    regs.tom.write(TOM_REG::RAW.val(0x1234));
    assert_eq!(regs.tom.get(), 0x1234);
}

#[test]
fn command_encodings_and_narrow_fields_are_unchanged() {
    assert_eq!(DCC_REG::COMMAND::Nop.value, 1 << 16);
    assert_eq!(DCC_REG::COMMAND::Precharge.value, 2 << 16);
    assert_eq!(DCC_REG::COMMAND::Mrs.value, 3 << 16);
    assert_eq!(DCC_REG::COMMAND::Emrs.value, 4 << 16);
    assert_eq!(DCC_REG::COMMAND::Cbr.value, 6 << 16);
    assert_eq!(DCC_REG::COMMAND::Normal.value, 7 << 16);
    assert_eq!(CPCTL_REG::PM_DIV.val(7).mask(), 0x3800);
    assert_eq!(DQSMT_REG::DQSMT_LO.val(15).mask(), 15);
    assert_eq!(HCTC_REG::HCTC_MODE.val(31).mask(), 31);
    assert_eq!(SLFRCS_REG::SR_CH1::SET.value, 2);
    assert_eq!(DEVEN_REG::D2F1::SET.value, 1 << 4);
}

#[test]
fn legacy_overlapping_timing_views_are_not_silently_narrowed() {
    // These intentionally retain the driver's established masks, including
    // overlapping assembly fields; migration is not a datasheet correction.
    assert_eq!(DRT0_REG::R_AP_TO_ACT.val(u32::MAX).mask(), 0xff);
    assert_eq!(DRT0_REG::W_AUTO_PCHG.val(u32::MAX).mask(), 0x1f0);
    assert_eq!(DRT1_REG::TRAS.val(u32::MAX).mask(), 0x07f8_0000);
    assert_eq!(DRT1_REG::RCVEN_COARSE.val(u32::MAX).mask(), 0x0f00_0000);
    assert_eq!(GBRCOMPCTL_REG::RCOMP_CFG27.val(3).mask(), 0x1800_0000);
    assert_eq!(EPVC1RCTL_REG::TC_VC_MAP.val(0x7f).mask(), 0xfe);
    assert_eq!(DMIVC1RCTL_REG::TC_VC_MAP.val(0x7f).mask(), 0xfe);
}
