// SPDX-License-Identifier: MIT OR Apache-2.0
//! The vendor CPM GPIO table ($A10) and the BMC ready pin.
use embedded_hal::delay::DelayNs;
use fstart_driver_amd::fch::gpio::{Control, Pad, Pull};
use fstart_driver_amd::soc::zeppelin::Zeppelin;
use fstart_platform_amd::noncar::BoardCtx;

/// The values are IOMUX functions, not named functions: the Zeppelin IOMUX
/// assignments are not known. Rows that only select a function leave the
/// pad configuration at its default.
pub const VENDOR: &[Pad] = &[
    Pad::driven(5, 0, true, Pull::None),
    Pad::driven(6, 0, true, Pull::None),
    Pad::native(9, 1, Pull::None),
    Pad::native(10, 2, Pull::None),
    Pad::native(17, 1, Pull::None),
    Pad::driven(23, 1, true, Pull::None),
    Pad::native(28, 0, Pull::None),
    Pad::native(40, 1, Pull::None),
    Pad::driven(42, 1, true, Pull::None),
    Pad::native(70, 0, Pull::None),
    Pad::native(76, 1, Pull::None),
    Pad::driven(88, 2, true, Pull::Up),
    Pad::native(117, 0, Pull::None),
    Pad::native(119, 2, Pull::None),
    Pad::native(120, 0, Pull::None),
    Pad::native(121, 0, Pull::None),
    Pad::native(122, 0, Pull::None),
    Pad::native(133, 0, Pull::None),
    Pad::native(135, 2, Pull::None),
    Pad::native(138, 0, Pull::None),
    Pad::native(140, 2, Pull::None),
    Pad::native(141, 1, Pull::None),
    Pad::native(142, 2, Pull::None),
    Pad::native(143, 1, Pull::None),
    Pad::native(144, 1, Pull::None),
    Pad::native(145, 2, Pull::None),
    Pad::native(146, 2, Pull::None),
    Pad::native(151, 1, Pull::None),
    Pad::native(152, 1, Pull::None),
];

/// Low when the BMC is up.
const BMC_READY: u8 = 149;
/// 500 ms each: the vendor "2 minutes" default.
const BMC_READY_POLLS: u32 = 240;

/// GbtVgaInitial waits for the BMC before it uses the AST2500 VGA: GPIO149
/// with debounce 5 on IOMUX function 1, then poll the pin.
pub fn wait_for_bmc(ctx: &mut BoardCtx<'_, Zeppelin>) {
    let ready = Pad::native_with(BMC_READY, 1, Control::DEBOUNCE.val(5));
    let mut clock = ctx.clock;
    let gpio = ctx.gpio();
    if gpio.configure(&[ready], &[]).is_err() {
        return;
    }
    let waited = (0..BMC_READY_POLLS).find(|&poll| {
        if gpio.get(BMC_READY) != Ok(true) {
            return true;
        }
        if poll == 0 {
            fstart_log::info!("Waiting for the BMC");
        }
        clock.delay_us(500_000);
        false
    });
    if waited.is_none() {
        fstart_log::warn!("BMC not ready after {} s", BMC_READY_POLLS / 2);
    }
}
