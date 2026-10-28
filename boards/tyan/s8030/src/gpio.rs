//! S8030 FCH GPIO: the CPM `$A10` table and the OEM pads that run before it
//! (BOARD_S8030 §2, §3.3, §3.4).
//!
//! Vendor order (AmdCpmTyanInitPeim / OemAmdCpmInitPeim, then
//! AmdCpmGpioInitPeim): the OEM callbacks first configure and read the M.2
//! detect pads ([`m2_detect_pads`]) and, on Rome, drive GPIO4/5
//! ([`rome_oem_pads`]); `$A10` ([`CPM_GPIO`]) is applied afterwards. The
//! CPM device tables (`$A12`–`$A17`) are empty, so there is no power/reset
//! sequencing and no CLKREQ table, and `$A11` (GEVENT) is empty.
//!
//! All pads are on socket 0's FCH; pins 256+ are its remote bank (the CPM
//! die-coded pin words are already remapped). Apply with
//! `fch::gpio::configure_all`, read with `fch::gpio::read`.

mod table;

pub use fstart_driver_amd::sp3::fch::gpio::{Direction, Pad, Pin, Pull};
pub use table::CPM_GPIO;

use fstart_driver_amd::sp3::soc::Generation;

/// M.2 CN1 (lanes 64–67) SATA-vs-PCIe detect pin.
pub const M2_CN1_DETECT: Pin = Pin(144);
/// M.2 CN3 (lanes 68–71) SATA-vs-PCIe detect pin.
pub const M2_CN3_DETECT: Pin = Pin(140);

/// The M.2 detect pads as the OEM code configures them before reading
/// (CN1, CN3). Rome uses no pull (`0x982`/`0xa82`, `ffe45e98`), Milan a
/// pull-up (`0x9a2`/`0xaa2`, `ffd97627`); `$A10` later sets both to the
/// pull-up form on either generation. A high level selects PCIe x4
/// ([`crate::dxio::M2Mode::from_detect`]).
pub const fn m2_detect_pads(generation: Generation) -> [Pad; 2] {
    match generation {
        Generation::Milan => [
            Pad::from_cpm(M2_CN1_DETECT, 0x9a2),
            Pad::from_cpm(M2_CN3_DETECT, 0xaa2),
        ],
        _ => [
            Pad::from_cpm(M2_CN1_DETECT, 0x982),
            Pad::from_cpm(M2_CN3_DETECT, 0xa82),
        ],
    }
}

/// Rome only: GPIO4/GPIO5 from the hidden Setup rows `0x1b7`/`0x1b8`
/// (default 1, `SUPPRESSIF TRUE`): `0x880 | out | pull` = output high with
/// pull-up (`ffe456da`). Their function is unknown (§10 Q3; X550
/// LAN-disable is a guess). Milan dropped the rows; its loop still runs
/// over the `$A10` header and issues `GpioWrite(0, 0)` and
/// `GpioWrite(0x4124, 0x3031)` (donor bug, §3.3), which is not reproduced.
pub const fn rome_oem_pads(generation: Generation) -> &'static [Pad] {
    const ROME: [Pad; 2] = [Pad::from_cpm(Pin(4), 0x8a1), Pad::from_cpm(Pin(5), 0x8a1)];
    match generation {
        Generation::Rome => &ROME,
        _ => &[],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpm_table_shape() {
        let pins: std::vec::Vec<u16> = CPM_GPIO.iter().map(|p| p.pin.0).collect();
        let mut unique = pins.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), CPM_GPIO.len(), "one row per pin");
        assert!(CPM_GPIO.iter().all(|p| p.pin.is_valid()));
        // Remote bank 256..=267 (die-coded pins of dies 1 and 2).
        let remote: std::vec::Vec<u16> = unique.iter().copied().filter(|&p| p >= 256).collect();
        assert_eq!(remote, (256..=267).collect::<std::vec::Vec<_>>());
        let row = |pin| *CPM_GPIO.iter().find(|p| p.pin == Pin(pin)).unwrap();
        // The M.2 detect pins end up as pulled-up inputs on their functions.
        assert_eq!(row(144), Pad::from_cpm(Pin(144), 0x9a2));
        assert_eq!(row(140), Pad::from_cpm(Pin(140), 0xaa2));
        // Board enables driven high / low (§3.4).
        let dir = |pins: &[u16]| {
            pins.iter()
                .map(|&p| row(p).direction)
                .collect::<std::vec::Vec<_>>()
        };
        assert!(
            dir(&[3, 6, 42, 29, 32, 259])
                .iter()
                .all(|&d| d == Direction::Output(true))
        );
        assert!(
            dir(&[256, 257])
                .iter()
                .all(|&d| d == Direction::Output(false))
        );
        // Every row sets the IOMUX and the pulls; GPIO4/5 are not in `$A10`.
        assert!(
            CPM_GPIO
                .iter()
                .all(|p| p.function.is_some() && p.pull.is_some())
        );
        assert!(!pins.contains(&4) && !pins.contains(&5));
    }

    #[test]
    fn oem_pads() {
        let [cn1, cn3] = m2_detect_pads(Generation::Rome);
        assert_eq!(
            (cn1.function, cn1.pull, cn1.direction),
            (Some(1), Some(Pull::None), Direction::Input)
        );
        assert_eq!((cn3.pin, cn3.function), (Pin(140), Some(2)));
        assert_eq!(m2_detect_pads(Generation::Milan)[0].pull, Some(Pull::Up));
        assert_eq!(rome_oem_pads(Generation::Milan), &[]);
        let gpio5 = rome_oem_pads(Generation::Rome)[1];
        assert_eq!(
            (gpio5.pin, gpio5.pull, gpio5.direction),
            (Pin(5), Some(Pull::Up), Direction::Output(true))
        );
    }
}
