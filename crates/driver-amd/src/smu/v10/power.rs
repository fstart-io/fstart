// SPDX-License-Identifier: MIT OR Apache-2.0
//! SMU policy v0x13 and persistent DMA pages.
use crate::ccx::tsc::DelayNs;
use crate::smn::Smn;
use crate::smu::v10 as smu;
use core::{
    cell::UnsafeCell,
    sync::atomic::{Ordering, compiler_fence},
};
use fstart_pci::ConfigRegionAccess;
use zerocopy::{
    FromBytes, FromZeros, Immutable, IntoBytes, KnownLayout, Unaligned,
    byteorder::{LittleEndian, U32},
};
#[derive(Debug, Clone, Copy, Default)]
pub struct Telemetry {
    pub slope_ma: u32,
    pub offset: i32,
}
#[derive(Debug, Clone, Copy, Default)]
pub struct Config {
    pub sustained_mw: u32,
    pub fast_mw: u32,
    pub slow_mw: u32,
    pub slow_seconds: u32,
    pub stapm_seconds: u32,
    pub thermal_c: u32,
    pub vrm_ma: u32,
    pub vrm_max_ma: u32,
    pub soc_vrm_ma: u32,
    pub soc_vrm_max_ma: u32,
    pub psi0_ma: u32,
    pub soc_psi0_ma: u32,
    pub prochot_ms: u32,
    pub system_config: u8,
    pub core_dldo_bypass: bool,
    pub tsi_alert_mode: bool,
    pub vdd_margin_mv: u32,
    pub soc_margin_mv: u32,
    pub telemetry: [Telemetry; 2],
    pub aclk_400mhz: bool,
    pub min_soc_vid_offset: u8,
}
#[repr(C)]
#[derive(Debug, Clone, Copy, FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned)]
pub struct WirePolicy {
    version: U32<LittleEndian>,
    sustained: U32<LittleEndian>,
    fast: U32<LittleEndian>,
    slow: U32<LittleEndian>,
    slow_time: U32<LittleEndian>,
    reserved014: [u8; 3],
    aa_mode: u8,
    stapm_time: U32<LittleEndian>,
    thermal: U32<LittleEndian>,
    vrm: U32<LittleEndian>,
    vrm_max: U32<LittleEndian>,
    soc_vrm: U32<LittleEndian>,
    soc_vrm_max: U32<LittleEndian>,
    psi0: U32<LittleEndian>,
    soc_psi0: U32<LittleEndian>,
    prochot: U32<LittleEndian>,
    system: u8,
    reserved03d: [u8; 16],
    gfx_margin: u8,
    reserved04e: [u8; 10],
    core_bypass: u8,
    reserved059: [u8; 6],
    tsi_alert: u8,
    vdd_margin: U32<LittleEndian>,
    soc_margin: U32<LittleEndian>,
    reserved068: [u8; 4],
    ccx_fuses: u8,
    gfx_fuses: u8,
    cac_fuses: u8,
    reserved06f: [u8; 0x6d],
    vdd_slope: U32<LittleEndian>,
    vdd_offset: U32<LittleEndian>,
    soc_slope: U32<LittleEndian>,
    soc_offset: U32<LittleEndian>,
    reserved0ec: [u8; 0x21],
    aclk400: u8,
    soc_vid: u8,
    reserved10f: [u8; 0x35],
}
const _: () = assert!(
    size_of::<WirePolicy>() == 0x144
        && core::mem::offset_of!(WirePolicy, system) == 0x3c
        && core::mem::offset_of!(WirePolicy, vdd_slope) == 0xdc
        && core::mem::offset_of!(WirePolicy, aclk400) == 0x10d
);
impl WirePolicy {
    pub fn build(config: Config) -> Self {
        let custom = config.fast_mw != 0
            && config.slow_mw != 0
            && config.slow_seconds != 0
            && config.stapm_seconds != 0;
        Self {
            version: U32::new(0x13),
            sustained: U32::new(config.sustained_mw),
            fast: U32::new(if custom { config.fast_mw } else { 1 }),
            slow: U32::new(if custom { config.slow_mw } else { 0 }),
            slow_time: U32::new(if custom { config.slow_seconds } else { 28 }),
            stapm_time: U32::new(if custom { config.stapm_seconds } else { 0 }),
            thermal: U32::new(config.thermal_c),
            vrm: U32::new(config.vrm_ma),
            vrm_max: U32::new(config.vrm_max_ma),
            soc_vrm: U32::new(config.soc_vrm_ma),
            soc_vrm_max: U32::new(config.soc_vrm_max_ma),
            psi0: U32::new(config.psi0_ma),
            soc_psi0: U32::new(config.soc_psi0_ma),
            prochot: U32::new(config.prochot_ms),
            system: config.system_config,
            gfx_margin: 5,
            core_bypass: u8::from(config.core_dldo_bypass),
            tsi_alert: u8::from(config.tsi_alert_mode),
            vdd_margin: U32::new(config.vdd_margin_mv),
            soc_margin: U32::new(config.soc_margin_mv),
            ccx_fuses: 1,
            gfx_fuses: 1,
            cac_fuses: 1,
            vdd_slope: U32::new(config.telemetry[0].slope_ma),
            vdd_offset: U32::new(config.telemetry[0].offset as u32),
            soc_slope: U32::new(config.telemetry[1].slope_ma),
            soc_offset: U32::new(config.telemetry[1].offset as u32),
            aclk400: u8::from(config.aclk_400mhz),
            soc_vid: config.min_soc_vid_offset,
            ..Self::new_zeroed()
        }
    }
}
#[repr(C, align(4096))]
pub struct Pages {
    bytes: UnsafeCell<[u8; 8192]>,
}
impl Default for Pages {
    fn default() -> Self {
        Self::new()
    }
}
impl Pages {
    pub const fn new() -> Self {
        Self {
            bytes: UnsafeCell::new([0; 8192]),
        }
    }
    fn prepare(&mut self, policy: &WirePolicy) -> Result<u32, Error> {
        let base = self.bytes.get() as usize;
        if base & 4095 != 0
            || base
                .checked_add(8192)
                .is_none_or(|end| end > u32::MAX as usize)
        {
            return Err(Error::Address);
        }
        // SAFETY: exclusive initial ownership, before any SMU DMA submission.
        let bytes = unsafe { &mut *self.bytes.get() };
        bytes.fill(0);
        bytes[4096..4096 + size_of::<WirePolicy>()].copy_from_slice(policy.as_bytes());
        compiler_fence(Ordering::SeqCst);
        Ok(base as u32)
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Address,
    Smu(smu::Error),
}
/// Native late silicon deliberately observes nonstandard completed responses
/// rather than equating them with timeouts. Report EVERY such status to the
/// caller; this exception never applies to early/DXIO/USB strict requests.
pub(crate) fn command(
    smn: &Smn<impl ConfigRegionAccess>,
    message: u32,
    args: [u32; 6],
    delay: &mut impl DelayNs,
    notice: &mut impl FnMut(u32, u32),
) -> Result<[u32; 6], Error> {
    let reply = smu::request_raw(smn, message, args, delay).map_err(Error::Smu)?;
    if reply.status != 1 {
        notice(message, reply.status);
    }
    Ok(reply.arguments)
}
/// # Safety
/// Both 4-KiB pages must be permanently reserved coherent identity-mapped
/// DMA RAM, excluded from all allocators/payload maps. The SMU continues using
/// the policy/scratch pages after return; retain them until reset, including
/// on any error. The consumed static borrow permits neither reclamation nor
/// retry. A nonstandard-response observer must emit/retain diagnostics.
pub unsafe fn install(
    smn: &Smn<impl ConfigRegionAccess>,
    pages: &'static mut Pages,
    config: Config,
    delay: &mut impl DelayNs,
    notice: &mut impl FnMut(u32, u32),
) -> Result<(), Error> {
    let base = pages.prepare(&WirePolicy::build(config))?;
    transfer(smn, base, delay, notice)
}

// Only install supplies this address in production, after DMA preflight and
// publication. Separating submission permits CPU-only mailbox transaction tests.
fn transfer(
    smn: &Smn<impl ConfigRegionAccess>,
    base: u32,
    delay: &mut impl DelayNs,
    notice: &mut impl FnMut(u32, u32),
) -> Result<(), Error> {
    command(smn, 3, [0x13, 0, 0, 0, 0, 0], delay, notice)?;
    let mut payload = command(smn, 0x13, [base + 4096, 0, 0, 0, 0, 0], delay, notice)?;
    // SET_POLICY_ADDRESS returns a context in the remaining five arguments.
    // TRANSFER_POLICY reuses those words; it is NOT a fresh zero payload.
    payload[0] = 0;
    command(smn, 0x12, payload, delay, notice)?;
    command(smn, 0x17, [0; 6], delay, notice)?;
    command(smn, 0x15, [base, 0, 0, 0, 0, 0], delay, notice)?;
    command(smn, 0x14, [0; 6], delay, notice)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    extern crate std;
    use crate::smn::testing::{SmnModel, SmnTest};
    use std::{collections::BTreeMap, vec::Vec};
    struct Mailbox {
        regs: BTreeMap<u32, u32>,
        frames: Vec<(u32, [u32; 6])>,
        response: u32,
        unavailable: Option<u32>,
    }
    impl Mailbox {
        fn new() -> Self {
            Self {
                regs: BTreeMap::new(),
                frames: Vec::new(),
                response: 1,
                unavailable: None,
            }
        }
    }
    impl SmnModel for Mailbox {
        fn read(&mut self, address: u32) -> u32 {
            if address == smu::RESPONSE.address() {
                self.response
            } else {
                self.regs.get(&address).copied().unwrap_or(0)
            }
        }
        fn write(&mut self, address: u32, value: u32) {
            self.regs.insert(address, value);
            if address == smu::RESPONSE.address() {
                self.response = value;
            }
            if address == smu::MESSAGE.address() {
                self.frames.push((
                    value,
                    core::array::from_fn(|i| {
                        self.regs[&smu::ARGUMENTS.offset(i as u32 * 4).address()]
                    }),
                ));
                self.response = if self.unavailable == Some(value) {
                    u32::MAX
                } else if value == 0x13 {
                    0xfe
                } else {
                    1
                };
                if value == 0x13 {
                    for (i, word) in [0x77, 0x1111, 0x2222, 0x3333, 0x4444, 0x5555]
                        .into_iter()
                        .enumerate()
                    {
                        self.regs
                            .insert(smu::ARGUMENTS.offset(i as u32 * 4).address(), word);
                    }
                }
            }
        }
    }
    #[test]
    fn transfer_reuses_set_address_context_and_surfaces_nonstandard_status() {
        let mailbox = SmnTest::new(Mailbox::new());
        let mut notices = Vec::new();
        transfer(
            &mailbox.smn(),
            0x02019000,
            &mut crate::ccx::tsc::testing::none(),
            &mut |message, status| notices.push((message, status)),
        )
        .unwrap();
        assert_eq!(
            mailbox.model().frames.as_slice(),
            &[
                (3, [0x13, 0, 0, 0, 0, 0]),
                (0x13, [0x0201a000, 0, 0, 0, 0, 0]),
                (0x12, [0, 0x1111, 0x2222, 0x3333, 0x4444, 0x5555]),
                (0x17, [0; 6]),
                (0x15, [0x02019000, 0, 0, 0, 0, 0]),
                (0x14, [0; 6]),
            ]
        );
        assert_eq!(notices.as_slice(), &[(0x13, 0xfe)]);
        let mailbox = SmnTest::new(Mailbox::new());
        mailbox.model().unavailable = Some(0x12);
        assert_eq!(
            transfer(
                &mailbox.smn(),
                0x02019000,
                &mut crate::ccx::tsc::testing::none(),
                &mut |_, _| {}
            ),
            Err(Error::Smu(smu::Error::Unavailable))
        );
        assert_eq!(
            mailbox
                .model()
                .frames
                .iter()
                .map(|frame| frame.0)
                .collect::<Vec<_>>(),
            [3, 0x13, 0x12]
        );
    }
    #[test]
    fn native_policy_offsets_signed_telemetry_and_all_or_nothing_custom_ppt() {
        let config = Config {
            sustained_mw: 4800,
            fast_mw: 9000,
            slow_mw: 6000,
            slow_seconds: 5,
            stapm_seconds: 1400,
            system_config: 1,
            telemetry: [
                Telemetry {
                    slope_ma: 32453,
                    offset: 168,
                },
                Telemetry {
                    slope_ma: 22644,
                    offset: -70,
                },
            ],
            ..Config::default()
        };
        let policy = WirePolicy::build(config);
        let bytes = policy.as_bytes();
        assert_eq!(&bytes[0..4], &0x13u32.to_le_bytes());
        assert_eq!(&bytes[8..12], &9000u32.to_le_bytes());
        assert_eq!(&bytes[0xe8..0xec], &(-70i32).to_le_bytes());
        assert_eq!(&bytes[0x6c..0x6f], &[1, 1, 1]);
        assert_eq!(bytes[0x3c], 1);
        assert_eq!(bytes[0x4d], 5);
        let defaults = WirePolicy::build(Config {
            stapm_seconds: 0,
            ..config
        });
        assert_eq!(defaults.fast.get(), 1);
        assert_eq!(defaults.slow.get(), 0);
        assert_eq!(defaults.slow_time.get(), 28);
        assert_eq!(defaults.stapm_time.get(), 0);
        assert_eq!(size_of::<Pages>(), 8192);
        assert_eq!(align_of::<Pages>(), 4096);
    }
}
