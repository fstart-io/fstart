//! DDR3 mode registers, JEDEC reset sequencing and exit from self-refresh.
use super::{
    Error, MchBar, mc,
    reut::{self, Calibration},
    spd::Topology,
    timings::Timings,
    *,
};
use fstart_arch::x86::udelay;

/// JEDEC MR1 RTT_NOM uses non-contiguous pins A9/A6/A2.
pub const fn rtt_nom(ohms: u16) -> Option<u16> {
    Some(match ohms {
        0 => 0,
        20 => 0x200,
        30 => 0x204,
        40 => 0x44,
        60 => 4,
        120 => 0x40,
        _ => return None,
    })
}

/// Values by MR number, channel and physical slot; reused after training.
#[derive(Default)]
pub struct ModeRegisters(pub [[[u16; 2]; 2]; 4]);
impl ModeRegisters {
    pub fn derive(t: &Topology, tm: Timings, ult: bool) -> Self {
        let mut m = Self::default();
        let cl = (tm.aa - 4) as u16;
        let wr = [1, 2, 3, 4, 5, 5, 6, 6, 7, 7, 0, 0][(tm.wr - 5) as usize];
        let mr0 = ((cl & 8) >> 1) | ((cl & 7) << 4) | (1 << 8) | (wr << 9);
        let asr = t.thermal & 4 != 0;
        let srt = t.thermal & 1 != 0 && !asr;
        let mr2 = (((tm.cwl - 5) as u16) << 3)
            | (u16::from(asr) << 6)
            | (u16::from(srt) << 7)
            | (if ult { 2 } else { 1 } << 9);
        for ch in t.channels() {
            for slot in 0..2 {
                if t.dimms[ch][slot].is_some() {
                    m.0[0][ch][slot] = mr0;
                    m.0[1][ch][slot] = 2 | if t.dpc[ch] == 2 { 1 << 2 } else { 0 };
                    m.0[2][ch][slot] = mr2;
                }
            }
        }
        m
    }
    fn program_channel(
        &self,
        mch: MchBar,
        t: &Topology,
        ch: usize,
        restore_mrs: bool,
    ) -> Result<(), Error> {
        for mr in [2, 3, 1, 0] {
            if mr == 2 && !restore_mrs {
                let slot = usize::from(t.dimms[ch][0].is_none());
                let word = u32::from(self.0[mr][ch][slot]);
                let present = (u32::from(t.rankmap[ch] & 1)) | (u32::from(t.rankmap[ch] & 4) >> 1);
                let mirror = u32::from(t.mirrored[ch]);
                let swizzle = (mirror >> 1 & 1) | (mirror >> 2 & 2);
                let srt = if word & (1 << 7) != 0 { present } else { 0 };
                mch.write32(
                    mc(0x429c, ch),
                    word & 0x073f
                        | (TC_MR2_SHADOW::SRT_AVAILABLE.val(srt)
                            + TC_MR2_SHADOW::ADDR_BIT_SWIZZLE.val(swizzle))
                        .value,
                );
            }
            reut::mrs_all(mch, t, ch, mr as u8, self.0[mr][ch])?;
        }
        reut::zq(mch, t, 1 << ch, Calibration::Init)
    }
}

/// ULT routes DRAM_RESET# via Lynx Point-LP RCBA PM_CFG2[26]; the
/// platform supplies that single signal, not a second register-access API.
pub enum ResetPin<'a> {
    SystemAgent,
    LynxPointLp(&'a mut dyn FnMut(bool)),
}
impl ResetPin<'_> {
    fn assert(&mut self, mch: MchBar, asserted: bool) {
        match self {
            Self::SystemAgent => mch.clrsetbits32(
                0x5030,
                MC_INIT_STATE_G::DDR_NOT_RESET::SET.value,
                MC_INIT_STATE_G::DDR_NOT_RESET
                    .val(u32::from(!asserted))
                    .value,
            ),
            Self::LynxPointLp(set) => set(asserted),
        }
    }
}
fn tick(mch: MchBar) {
    for _ in 0..2 {
        let _ = mch.read32(0x4ce0);
    }
}
/// Must not be called on S3: the DIMMs must remain in self-refresh.
pub fn jedec_init(
    mch: MchBar,
    t: &Topology,
    mrs: &ModeRegisters,
    restore_mrs: bool,
    pin: &mut ResetPin<'_>,
) -> Result<(), Error> {
    sync_io(mch)?;
    if reset_and_program(mch, t, mrs, restore_mrs, pin)? != 0 {
        return Err(Error::Reut);
    }
    Ok(())
}

pub(super) fn sync_io(mch: MchBar) -> Result<(), Error> {
    io_reset(mch)?;
    wait_for_first_rcomp(mch)
}

/// After DDRIO reset and first RCOMP, pulse DRAM_RESET# and initialize each
/// channel. Return completed command failures by channel; timeouts are fatal.
pub(super) fn reset_and_program(
    mch: MchBar,
    t: &Topology,
    mrs: &ModeRegisters,
    restore_mrs: bool,
    pin: &mut ResetPin<'_>,
) -> Result<u8, Error> {
    mch.write32(0x4d94, REUT_MISC_ODT_CTRL::ODT_OVERRIDE.val(15).value);
    if matches!(pin, ResetPin::LynxPointLp(_)) {
        pin.assert(mch, false);
    }
    let state =
        (MC_INIT_STATE_G::DDR_NOT_RESET::SET + MC_INIT_STATE_G::SAFE_SELF_REFRESH::SET).value;
    mch.write32(0x5030, state);
    let override_cke = REUT_MISC_CKE_CTRL::CKE_OVERRIDE.val(15).value;
    mch.write32(0x4d90, override_cke);
    pin.assert(mch, true);
    udelay(200);
    pin.assert(mch, false);
    udelay(500);
    mch.write32(0x5030, state | MC_INIT_STATE_G::DCLK_ENABLE::SET.value);
    tick(mch);
    for ch in 0..2 {
        mch.write32(
            mc(0x4190, ch),
            override_cke | REUT_MISC_CKE_CTRL::CKE_ON.val(t.rankmap[ch].into()).value,
        );
    }
    tick(mch);
    let mut failed = 0;
    let result =
        t.channels()
            .try_for_each(|ch| match mrs.program_channel(mch, t, ch, restore_mrs) {
                Err(Error::Reut) => {
                    failed |= 1 << ch;
                    Ok(())
                }
                result => result,
            });
    mch.write32(0x4d94, 0);
    result.map(|()| failed)
}

pub fn exit_selfrefresh(mch: MchBar, t: &Topology) -> Result<(), Error> {
    for ch in t.channels() {
        let dq0 = mch.read32(0x74 + ch as u32 * 0x100) | DDR_DATA_CONTROL_0::READ_RF_RD::SET.value;
        for rank in t.populated_ranks(ch) {
            mch.write32(
                0x3074 + ch as u32 * 0x100,
                DDR_DATA_CONTROL_0::READ_RF_RANK
                    .val(rank as u32)
                    .modify(dq0),
            );
        }
    }
    udelay(6);
    for ch in 0..2 {
        mch.write32(
            mc(0x4190, ch),
            REUT_MISC_CKE_CTRL::CKE_ON.val(t.rankmap[ch].into()).value,
        );
    }
    mch.write32(0x4d94, 0);
    for ch in t.channels() {
        match reut::zq(mch, t, 1 << ch, Calibration::Long) {
            Err(Error::Reut) => {
                fstart_log::warn!("Haswell: CH{} ZQ_LONG error on self-refresh exit", ch)
            }
            result => result?,
        }
    }
    Ok(())
}
