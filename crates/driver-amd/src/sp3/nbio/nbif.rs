//! NBIF: the internal "bus interface" bridges that expose the NBIO's
//! integrated functions (PTDMA, PSP/CCP, SATA, USB, HDA, NTB), their
//! straps, bus numbers and the HW-init lock (NBIO_SMU §3.4, §3.5).
//!
//! Each NBIO has three NBIFs. On Rome/Milan NBIF `k` of quadrant `n` is the
//! SMN aperture `0x101 + 4k + n` (`<< 20`); on Naples NBIF `k` of every die
//! is `0x101 + k`.

use fstart_pci::ConfigRegionAccess;
use tock_registers::interfaces::{ReadWriteable, Readable};
use tock_registers::register_bitfields;

use super::{Nbio, NbioPolicy};
use crate::smn::SmnAddr;
use crate::sp3::soc::Generation;

/// NBIFs per NBIO.
pub const NBIFS: u8 = 3;

/// `NBIF_FUNC_STRAP0` of device 0..2, function 0..7 (stride 0x1000/0x200).
pub const fn func_strap0(dev: u8, func: u8) -> u32 {
    0x3_4000 + dev as u32 * 0x1000 + func as u32 * 0x200
}
/// `NBIF_INTR_LINE_EN`: one bit per `dev * 8 + func`.
pub const INTR_LINE_EN: u32 = 0x3_A008;
/// `NBIF_BIFC_MISC_CTL0`.
pub const BIFC_MISC_CTL0: u32 = 0x3_A010;
/// `NBIF_MGCG_CTL_LCLK`.
pub const MGCG_CTL_LCLK: u32 = 0x3_A21C;
/// HW-init lock (`BIFC_HW_INIT_LOCK`-style; bit 0 locks the straps).
pub const HW_INIT_LOCK: u32 = 0x2_358C;
/// Rome/Milan port strap carrying the internal bridge's devfn and bus,
/// port `p` at `+ 0x200 * p`.
pub const fn port_bus_strap(port: u8) -> u32 {
    0x3_101C + port as u32 * 0x200
}
/// Naples: bus number strap (`[31:24]`).
pub const ZP_BUS_STRAP: u32 = 0x3_1404;
/// Naples parity-error workaround register.
pub const ZP_PARITY: u32 = 0x3_9004;

register_bitfields![u32,
    pub FUNC_STRAP0 [
        /// The function exists (is exposed on the NBIF's internal bus).
        EXIST OFFSET(28) NUMBITS(1) []
    ],
    pub BUS_STRAP [
        DEVFN OFFSET(24) NUMBITS(8) [],
        BUS OFFSET(16) NUMBITS(8) []
    ],
    pub ZP_BUS [
        BUS OFFSET(24) NUMBITS(8) []
    ],
    pub MGCG [
        /// Clock-gating enable.
        EN OFFSET(0) NUMBITS(1) [],
        /// Hysteresis / mode field AGESA writes from PCDs.
        MODE OFFSET(2) NUMBITS(8) []
    ],
    pub LOCK [
        LOCK OFFSET(0) NUMBITS(1) []
    ]
];

impl<P: ConfigRegionAccess + Clone> Nbio<P> {
    /// SMN base of NBIF `k` of this NBIO.
    pub fn nbif(&self, k: u8) -> SmnAddr {
        debug_assert!(k < NBIFS);
        let k = k as u32;
        match self.generation {
            Generation::Naples => 0x1010_0000 + (k << 20),
            Generation::Rome | Generation::Milan => self.at(0x1010_0000 + (k << 22)),
        }
    }

    /// Whether NBIF `k` exposes `dev`.`func` (FUNC_STRAP0 EXIST).
    pub fn nbif_function_exists(&self, k: u8, dev: u8, func: u8) -> bool {
        self.reg_at::<FUNC_STRAP0::Register>(self.nbif(k) + func_strap0(dev, func))
            .is_set(FUNC_STRAP0::EXIST)
    }

    /// Program the NBIF internal bus numbers (NBIO_SMU §3.5 "NBIF
    /// bus/devfn"): Naples `0x31404[31:24]` of all three NBIFs; Rome only
    /// NBIF1; Milan all three with devfn `0x27/0x28/0x29 + 0x20·port`,
    /// skipping NBIF2 port 0 when the NTB pair is set.
    pub fn set_nbif_bus_numbers(&self, policy: &NbioPolicy) {
        let bus = self.bus() as u32;
        match self.generation {
            Generation::Naples => (0..NBIFS).for_each(|k| {
                self.reg_at::<ZP_BUS::Register>(self.nbif(k) + ZP_BUS_STRAP)
                    .modify(ZP_BUS::BUS.val(bus))
            }),
            Generation::Rome | Generation::Milan => {
                let nbifs: &[u8] = if self.generation == Generation::Rome {
                    &[1]
                } else {
                    &[0, 1, 2]
                };
                nbifs
                    .iter()
                    .flat_map(|&k| (0..3u8).map(move |p| (k, p)))
                    .filter(|&(k, p)| !(k == 2 && p == 0 && policy.milan.nbif2_ntb))
                    .for_each(|(k, p)| {
                        let devfn = 0x27 + k as u32 + 0x20 * p as u32;
                        self.reg_at::<BUS_STRAP::Register>(self.nbif(k) + port_bus_strap(p))
                            .modify(BUS_STRAP::DEVFN.val(devfn) + BUS_STRAP::BUS.val(bus));
                    });
            }
        }
    }

    /// Set the NBIF HW-init lock (DXE, first PciIo install, after PCI
    /// enumeration; NBIO_SMU §3.5). AGESA records these writes in the S3
    /// boot script. Milan also locks `0x105313AC/0x105315AC` bit 0 (NBIF1
    /// port registers) and writes NBIF2's lock a second time unless PCD 0x70
    /// (idempotent, done once here).
    pub fn nbif_hw_init_lock(&self) {
        (0..NBIFS).for_each(|k| {
            self.reg_at::<LOCK::Register>(self.nbif(k) + HW_INIT_LOCK)
                .modify(LOCK::LOCK::SET)
        });
        if self.generation == Generation::Milan {
            [0x3_13AC, 0x3_15AC].iter().for_each(|&r| {
                self.reg_at::<LOCK::Register>(self.nbif(1) + r)
                    .modify(LOCK::LOCK::SET)
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sp3::nbio::tests::nbio;

    #[test]
    fn nbif_addresses() {
        let (_, r) = nbio(Generation::Rome, 1, 0);
        assert_eq!(r.nbif(0), 0x1020_0000);
        assert_eq!(r.nbif(1), 0x1060_0000);
        assert_eq!(r.nbif(2), 0x10A0_0000);
        let (_, z) = nbio(Generation::Naples, 1, 0);
        assert_eq!(z.nbif(2), 0x1030_0000);
        assert_eq!(func_strap0(1, 0), 0x3_5000);
    }

    #[test]
    fn bus_numbers_per_generation() {
        let (m, n) = nbio(Generation::Milan, 2, 0x60);
        let ntb = NbioPolicy {
            milan: crate::sp3::nbio::MilanPolicy {
                nbif2_ntb: true,
                ..Default::default()
            },
            ..Default::default()
        };
        n.set_nbif_bus_numbers(&ntb);
        let s = m.0.borrow();
        assert_eq!(s.smn[&(0x60, 0x1033_121C)], 0x4760_0000, "NBIF0 port 1");
        assert_eq!(s.smn[&(0x60, 0x10B3_141C)], 0x6960_0000, "NBIF2 port 2");
        assert!(!s.smn.contains_key(&(0x60, 0x10B3_101C)), "NTB skip");
        drop(s);

        let (m, n) = nbio(Generation::Rome, 0, 0x20);
        n.set_nbif_bus_numbers(&NbioPolicy::default());
        assert_eq!(m.0.borrow().log.len(), 3, "NBIF1 only");
    }

    #[test]
    fn hw_init_lock_milan() {
        let (m, n) = nbio(Generation::Milan, 0, 0);
        n.nbif_hw_init_lock();
        let s = m.0.borrow();
        for a in [
            0x1012_358C,
            0x1052_358C,
            0x1092_358C,
            0x1053_13AC,
            0x1053_15AC,
        ] {
            assert_eq!(s.smn[&(0, a)], 1, "{a:#x}");
        }
    }
}
