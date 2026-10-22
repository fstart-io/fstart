// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native ACP shutdown before DXIO. Register names from
//! Linux's ACP 3.x driver.
use super::registers::{Smn, SmnReg};
use fstart_pci::ConfigRegionAccess;
use tock_registers::{
    fields::FieldValue,
    interfaces::{ReadWriteable, Readable, Writeable},
    register_bitfields,
};

register_bitfields![u32,
    PgfsmControl [POWER_ON OFFSET(0) NUMBITS(1) []],
    PgfsmStatus [STATE OFFSET(0) NUMBITS(2) [On = 0]],
    SoftReset [AUDIO OFFSET(0) NUMBITS(1) [], AUDIO_DONE OFFSET(16) NUMBITS(1) []],
    Control [CLK_EN OFFSET(0) NUMBITS(1) []],
    DynamicCgMasterControl [ENABLE OFFSET(0) NUMBITS(16) []],
    PgmemCtrl [SHUT_DOWN OFFSET(16) NUMBITS(12) []],
    MemPgStatus [SHUT_DOWN OFFSET(0) NUMBITS(12) []],
    /// NBIF strap of the ACP function.
    NbifStrap [ENABLE OFFSET(28) NUMBITS(1) []]
];
const SOFT_RESET: SmnReg<SoftReset::Register> = SmnReg::new(0x0124_1000);
const CONTROL: SmnReg<Control::Register> = SmnReg::new(0x0124_1004);
const DYNAMIC_CG_MASTER_CONTROL: SmnReg<DynamicCgMasterControl::Register> =
    SmnReg::new(0x0124_1010);
const PGFSM_CONTROL: SmnReg<PgfsmControl::Register> = SmnReg::new(0x0124_141c);
const PGFSM_STATUS: SmnReg<PgfsmStatus::Register> = SmnReg::new(0x0124_1420);
const PGMEM_CTRL: SmnReg<PgmemCtrl::Register> = SmnReg::new(0x0124_18c0);
const MEM_PG_STS: SmnReg<MemPgStatus::Register> = SmnReg::new(0x0124_18cc);
const NBIF_STRAP: SmnReg<NbifStrap::Register> = SmnReg::new(0x1013_4a00);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Error {
    pub register: u32,
    pub value: u32,
    pub mask: u32,
    pub expected: u32,
}
fn wait<R: tock_registers::RegisterLongName>(
    smn: &Smn<impl ConfigRegionAccess>,
    reg: SmnReg<R>,
    expected: FieldValue<u32, R>,
    delay_us: &mut impl FnMut(u32),
) -> Result<(), Error> {
    let (mask, expected) = (expected.mask(), expected.value);
    let mut value = 0;
    for _ in 0..1000 {
        value = smn.reg(reg).get();
        if value == u32::MAX {
            break;
        }
        if value & mask == expected {
            return Ok(());
        }
        delay_us(1);
    }
    Err(Error {
        register: reg.address(),
        value,
        mask,
        expected,
    })
}
/// Disabled ACP must still be powered and taken out of reset to shut down
/// its SRAM. A timeout never proceeds to hide the unquiesced function.
pub fn power_off(
    smn: &Smn<impl ConfigRegionAccess>,
    enabled: bool,
    delay_us: &mut impl FnMut(u32),
) -> Result<(), Error> {
    if enabled {
        return Ok(());
    }
    smn.reg(PGFSM_CONTROL).modify(PgfsmControl::POWER_ON::SET);
    wait(smn, PGFSM_STATUS, PgfsmStatus::STATE::On, delay_us)?;
    smn.reg(CONTROL).modify(Control::CLK_EN::SET);
    smn.reg(SOFT_RESET).modify(SoftReset::AUDIO::SET);
    wait(smn, SOFT_RESET, SoftReset::AUDIO_DONE::SET, delay_us)?;
    smn.reg(SOFT_RESET).set(0);
    wait(
        smn,
        SOFT_RESET,
        SoftReset::AUDIO::CLEAR + SoftReset::AUDIO_DONE::CLEAR,
        delay_us,
    )?;
    smn.reg(DYNAMIC_CG_MASTER_CONTROL)
        .modify(DynamicCgMasterControl::ENABLE.val(0x1ff));
    smn.reg(PGMEM_CTRL)
        .modify(PgmemCtrl::SHUT_DOWN.val(0xfff));
    wait(smn, MEM_PG_STS, MemPgStatus::SHUT_DOWN.val(0), delay_us)?;
    smn.reg(DYNAMIC_CG_MASTER_CONTROL).set(0);
    smn.reg(CONTROL).set(0);
    smn.reg(NBIF_STRAP).modify(NbifStrap::ENABLE::CLEAR);
    Ok(())
}
