// SPDX-License-Identifier: MIT OR Apache-2.0
//! Peppy's variant Makefile and romstage: the part number repeats at 4–7,
//! but the channel population is not determined by the high bit on prototypes.
use fstart_core::services::ServiceError;
use fstart_platform_intel::haswell::{LpGpio, Slot, Slots};

const PARTS: [&[u8; 256]; 4] = [
    include_bytes!("../spd/Micron_4KTF25664HZ.spd"),
    include_bytes!("../spd/Hynix_HMT425S6AFR6A.spd"),
    include_bytes!("../spd/Elpida_EDJ4216EFBG.spd"),
    include_bytes!("../spd/Hynix_HMT425S6CFR6A.spd"),
];

pub(super) fn population(index: u8, version: u16) -> Slots {
    let part = Slot::MemoryDown(PARTS[usize::from(index & 3)]);
    let dual = if version == 0 { index != 0 } else { index < 4 };
    [
        [part, Slot::Empty],
        [if dual { part } else { Slot::Empty }, Slot::Empty],
    ]
}

pub(super) fn select() -> Result<Slots, ServiceError> {
    let gpio = LpGpio::default_base();
    let index = u8::from(gpio.get(13)) | u8::from(gpio.get(9)) << 1 | u8::from(gpio.get(47)) << 2;
    crate::slippy::post(0xd0 | index);
    let version =
        crate::slippy::ec().and_then(|mut ec| ec.board_version(&fstart_arch::x86::Udelay));
    Ok(select_revision(index, version, crate::slippy::post))
}

fn select_revision(
    index: u8,
    version: Result<u16, fstart_driver_chromeec::Error>,
    mut post: impl FnMut(u8),
) -> Slots {
    let version = match version {
        Ok(v) => {
            post(if v == 0 { 0xd8 } else { 0xd9 });
            fstart_log::info!("Peppy: RAM ID {}, EC board version {}", index, v);
            v
        }
        Err(e) => {
            if !(1..=3).contains(&index) {
                post(0xdf);
                fstart_log::warn!(
                    "Peppy: AMBIGUOUS RAM ID {}: EC GET_BOARD_VERSION {} ({}); assuming PROTO population",
                    index,
                    e.description(),
                    e.result_code()
                );
            } else {
                fstart_log::warn!(
                    "Peppy: EC GET_BOARD_VERSION {}; RAM ID {} always dual channel",
                    e.description(),
                    index
                );
            }
            0
        }
    };
    population(index, version)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_revision_uses_proto_but_flags_only_ambiguous_ids() {
        for i in 0..8 {
            let mut code = None;
            let slots = select_revision(i, Err(fstart_driver_chromeec::Error::Timeout), |p| {
                code = Some(p)
            });
            assert_eq!(code, (!(1..=3).contains(&i)).then_some(0xdf));
            assert_eq!(matches!(slots[1][0], Slot::MemoryDown(_)), i != 0);
        }
    }
}
