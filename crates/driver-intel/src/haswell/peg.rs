//! CPU PCI Express graphics root-port slot policy and R/WO capabilities.

use fstart_pci::ecam::EcamDevice;

/// Per-port policy for Haswell CPU PEG root ports (`00:01.0`..`00:01.2`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Peg {
    /// True when the downstream device is soldered onboard (slot not implemented).
    pub onboard: bool,
    /// Physical slot number (`1..=8191`); `0` defaults to `port_index + 1`.
    pub physical_slot: u16,
    /// Slot power-limit scale (`0..=3`, PCIe Slot Capabilities bits 16:15).
    pub power_limit_scale: u8,
    /// Slot power-limit value (`0` defaults to 75 W).
    pub power_limit_value: u8,
}

impl Peg {
    /// Default 75 W numbered add-in slot policy.
    pub const DEFAULT: Self = Self {
        onboard: false,
        physical_slot: 0,
        power_limit_scale: 0,
        power_limit_value: 0,
    };

    fn slot(self, n: u8, prev: u32) -> u32 {
        let number = if self.onboard {
            0
        } else if self.physical_slot != 0 {
            self.physical_slot
        } else {
            u16::from(n) + 1
        };
        let limit = if self.power_limit_value == 0 {
            75
        } else {
            self.power_limit_value
        };
        (prev & !((0x1fff << 19) | (3 << 15) | (255 << 7)))
            | (u32::from(number) << 19)
            | (u32::from(self.power_limit_scale) << 15)
            | (u32::from(limit) << 7)
    }
}

/// Apply slot capabilities, error clears, and link tuning to enabled PEG ports,
/// hiding ports configured as `None` in `DEVEN` (`00:00.0 + 0x54`).
pub(super) fn setup(ports: &[Option<Peg>; 3]) {
    let host = EcamDevice::new(0, 0, 0);
    for (n, policy) in ports.iter().enumerate() {
        let n = n as u8;
        let Some(p) = policy else {
            host.and32(0x54, !(8 >> n));
            continue;
        };
        let d = EcamDevice::new(0, 1, n);
        if !d.is_present() {
            continue;
        }
        d.write16(
            0xa2,
            (d.read16(0xa2) & !(1 << 8)) | (u16::from(!p.onboard) << 8),
        );
        d.write32(0xb4, p.slot(n, d.read32(0xb4)));
        for o in [6, 0x1e, 0xaa] {
            d.write16(o, 0xffff);
        }
        for o in [0x1c4, 0x1d0, 0x1f0] {
            d.write32(o, u32::MAX);
        }
        for (o, bits) in [
            (0x114, 0xfe),
            (0xc4, 1 << 19),
            (0x1cc, 1 << 14),
            (0xd0, 64),
            (0x22c, 1 << 31),
        ] {
            d.or32(o, bits);
        }
        d.modify32(0x250, !(7 << 20), 2 << 20);
        d.or32(0x238, 1 << 29);
        d.or32(0x1f8, 1 << 16);
        d.modify32(0xc28, !31, 0x13);
        d.or32(0xa4, 0);
        if n == 0 {
            d.or32(0xcd0, 1 << 11);
        }
        d.or32(0xac, 3 << 10);
        d.and32(0x200, !(3 << 26));
        d.or16(0x258, 4);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_a_75w_numbered_slot() {
        assert_eq!(Peg::DEFAULT.slot(2, 0), (3 << 19) | (75 << 7));
        assert_eq!(
            Peg {
                onboard: true,
                ..Peg::DEFAULT
            }
            .slot(0, 0),
            75 << 7
        );
    }
}
