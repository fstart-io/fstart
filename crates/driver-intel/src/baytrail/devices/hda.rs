//! HD Audio (00:1b.0) with the SoC's HDMI codec (coreboot `hda.c`).

use super::{Bar, Function};
use crate::baytrail::iosf::{Iosf, PUNIT};
use crate::baytrail::punit;
use crate::southbridge::hda::{HdaConfig, HdaController, HdaVerbTable};
use fstart_pci::ConfigRegionAccess;

/// The HDMI codec's pin widgets 5-7 (ports B-D), sent as the reference
/// sends them: raw verbs addressed to codec 2.
pub const HDMI_CODEC: HdaVerbTable = HdaVerbTable::new(0x8086_2882, 0)
    .extra_verb(0x2047_1c10)
    .extra_verb(0x2047_1d00)
    .extra_verb(0x2047_1e56)
    .extra_verb(0x2047_1f18)
    .extra_verb(0x2057_1c20)
    .extra_verb(0x2057_1d00)
    .extra_verb(0x2057_1e56)
    .extra_verb(0x2057_1f18)
    .extra_verb(0x2067_1c30)
    .extra_verb(0x2067_1d00)
    .extra_verb(0x2067_1e56)
    .extra_verb(0x2067_1f58);

pub fn init<P: ConfigRegionAccess>(iosf: &Iosf<P>) {
    let hda = Function::Hda.device();
    if !hda.is_present() {
        return;
    }
    // No-snoop traffic, HDMI codec connection.
    hda.or16(0x78, 1 << 11);
    hda.or32(0xc4, 1 << 1);
    hda.or8(0x43, 1 << 3 | 1 << 6);
    iosf.write(PUNIT, punit::PWRGT_CONTROL.address(), 0xc0);
    iosf.write(PUNIT, punit::PWRGT_CONTROL.address(), 0x00);
    // Internal settings, docking off.
    hda.or32(0xc0, 0x7 << 21);
    hda.or32(0xc4, 0x3 << 26 | 1 << 13 | 1 << 10);
    hda.write32(0xc8, 0x82a3_0000);
    hda.and32(0xd0, !(1 << 31));
    hda.and8(0x4d, !(1 << 7));

    let Some(Bar(base)) = Bar::of(&hda) else {
        return;
    };
    let controller = HdaController::new(base);
    let codecs = controller.detect_codecs();
    fstart_log::debug!("baytrail: HDA codec mask {:#x}", codecs);
    if codecs != 0 {
        controller.program_verb_tables(&HdaConfig::new().verb(HDMI_CODEC), codecs);
    }
}
