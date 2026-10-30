// SPDX-License-Identifier: MIT OR Apache-2.0
//! P-states and C-states of the Zen cores as the OS gets them: the enabled
//! P-states of PStateDef (`_PSS`), the C-state I/O of CStateBaseAddr
//! (`_CST`), and with the `acpi` feature the power objects of every
//! processor device ([`PowerAml`]). The formulas are the vendor CCX code's
//! P-state service, which coreboot's `cpu_power_state.c` shares: MHz from
//! FID/DID, SVI2 voltage, power from IddValue/IddDiv.
use super::msr::{CSTATE_BASE_ADDR, CSTATE_CONFIG, CstateBaseAddr, CstateConfig};
use fstart_arch::x86::cpu::amd::msr::{
    PSTATE_0, PSTATE_CURRENT_LIMIT, PstateCurLim, PstateDef, pstate_mhz,
};
use fstart_arch::x86::cpu::msr::MsrAccess;
use heapless::Vec;
use tock_registers::LocalRegisterCopy;
use tock_registers::interfaces::Readable;

/// One `_PSS` entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pstate {
    pub index: u8,
    pub mhz: u32,
    pub microvolts: u32,
    pub milliwatts: u32,
}

/// Decode a PStateDef value (formulas of the donors' P-state protocol).
pub fn decode_pstate(index: u8, raw: u64) -> Option<Pstate> {
    let r = LocalRegisterCopy::<u64, PstateDef::Register>::new(raw);
    if !r.is_set(PstateDef::PSTATE_EN) {
        return None;
    }
    let mhz = pstate_mhz(raw)?;
    // SVI2: 1.55 V − VID × 6.25 mV.
    let microvolts = 0xf8u32.saturating_sub(r.read(PstateDef::CPU_VID) as u32) * 6250;
    let div = [100u64, 1000, 10000].get(r.read(PstateDef::IDD_DIV) as usize)?;
    let milliwatts = (u64::from(microvolts) / 10 * r.read(PstateDef::IDD_VALUE) / div) as u32;
    Some(Pstate {
        index,
        mhz,
        microvolts,
        milliwatts,
    })
}

/// The enabled P-states up to PstateMaxVal, for `_PSS`.
pub fn pstates<M: MsrAccess + ?Sized>(cpu: &M) -> Vec<Pstate, 8> {
    let max = cpu
        .reg(PSTATE_CURRENT_LIMIT)
        .read(PstateCurLim::PSTATE_MAX_VAL) as u8;
    (0..=max)
        .filter_map(|i| decode_pstate(i, cpu.reg(PSTATE_0.offset(i.into())).get()))
        .collect()
}

/// What `_CST` needs (AmdCcx C-state info protocol).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CstateInfo {
    /// The I/O range trapped as C-state requests; C2 is a read of
    /// `io_base + 1`.
    pub io_base: u16,
    pub cc6: bool,
}

/// The C-states the CCX init left on the executing thread: none without a
/// C-state I/O base.
pub fn cstate_info<M: MsrAccess + ?Sized>(cpu: &M) -> Option<CstateInfo> {
    let io_base = cpu.reg(CSTATE_BASE_ADDR).read(CstateBaseAddr::CSTATE_ADDR) as u16;
    (io_base != 0).then(|| CstateInfo {
        io_base,
        cc6: cpu.reg(CSTATE_CONFIG).is_set(CstateConfig::CCR0_CC6EN),
    })
}

/// The power objects every processor device gets.
#[derive(Debug, Clone, Copy)]
pub struct Power<'a> {
    pub pstates: &'a [Pstate],
    pub cstates: Option<CstateInfo>,
    /// 2 with SMT, else 1 (`_PSD` domain = APIC ID / threads per core).
    pub threads_per_core: u8,
    /// C2 (CC6) exit latency: 400 µs on Zen, 18 µs on Zen 2 and 3.
    pub c2_latency_us: u16,
}

#[cfg(feature = "acpi")]
pub use aml::PowerAml;

#[cfg(feature = "acpi")]
mod aml {
    extern crate alloc;
    use super::Power;
    use alloc::vec::Vec;
    use fstart_acpi::gas::{AccessSize, AddressSpace, GAS};
    use fstart_acpi::processor::{
        Coordination, PerformanceState, PowerState, cst_aml, pct_aml, psd_aml, pss_aml,
    };

    /// `_PCT`/`_PSS`, `_CST` (C1 MWAIT through FFH, C2 an I/O read of the
    /// C-state base + 1, as AGESA's CPU SSDT and coreboot have them) and the
    /// per-core `_PSD` of one processor.
    pub struct PowerAml {
        shared: Vec<u8>,
        threads_per_core: u8,
    }

    impl PowerAml {
        pub fn new(power: &Power) -> Self {
            let pss: Vec<PerformanceState> = power
                .pstates
                .iter()
                .map(|p| PerformanceState {
                    mhz: p.mhz,
                    milliwatts: p.milliwatts,
                    latency_us: 0,
                    bus_master_latency_us: 0,
                    control: p.index.into(),
                    status: p.index.into(),
                })
                .collect();
            let ffh = |width, offset, access, address| {
                GAS::new(
                    AddressSpace::FunctionalFixedHardware,
                    width,
                    offset,
                    access,
                    address,
                )
            };
            // The P-state control MSR; the OS writes the _PSS control value.
            let pct = pct_aml(
                ffh(0, 0, AccessSize::Undefined, 0xC001_0062),
                ffh(0, 0, AccessSize::Undefined, 0),
            );
            let cst = power.cstates.map(|c| {
                cst_aml(&[
                    PowerState {
                        // FFixedHW class 2: MWAIT.
                        register: ffh(2, 2, AccessSize::ByteAccess, 0),
                        kind: 1,
                        latency_us: 1,
                        milliwatts: 0,
                    },
                    PowerState {
                        register: GAS::new(
                            AddressSpace::SystemIo,
                            8,
                            0,
                            AccessSize::ByteAccess,
                            u64::from(c.io_base) + 1,
                        ),
                        kind: 2,
                        latency_us: power.c2_latency_us,
                        milliwatts: 0,
                    },
                ])
            });
            let shared = [
                if pss.is_empty() {
                    Vec::new()
                } else {
                    [pct, pss_aml(&pss)].concat()
                },
                cst.unwrap_or_default(),
            ]
            .concat();
            Self {
                shared,
                threads_per_core: power.threads_per_core.max(1),
            }
        }

        /// The objects of the processor with `apic_id`: the threads of a
        /// core share their P-state domain.
        pub fn for_cpu(&self, apic_id: u32) -> Vec<u8> {
            let t = u32::from(self.threads_per_core);
            [
                self.shared.clone(),
                psd_aml(apic_id / t, Coordination::HwAll, t),
            ]
            .concat()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fstart_arch::x86::cpu::msr::testing::MsrSpace;

    #[test]
    fn pstate_decode() {
        // FID 0x8c, DID 8, VID 0x48, Idd 0x5a / 10: 3500 MHz, 1.1 V.
        let raw = 1 << 63 | 1 << 30 | 0x5a << 22 | 0x48 << 14 | 8 << 8 | 0x8c;
        let p = decode_pstate(0, raw).unwrap();
        assert_eq!((p.mhz, p.microvolts, p.milliwatts), (3500, 1_100_000, 9900));
        assert!(decode_pstate(1, 0x8c).is_none());
        let cpu = MsrSpace::new([(0xc001_0061, 0x10), (0xc001_0064, raw), (0xc001_0065, raw)]);
        assert_eq!(pstates(&cpu).len(), 2);
    }

    #[test]
    fn cstates_from_the_msrs() {
        let cpu = MsrSpace::new([(0xc001_0073, 0x813), (0xc001_0296, 0x48_4848)]);
        assert_eq!(
            cstate_info(&cpu),
            Some(CstateInfo {
                io_base: 0x813,
                cc6: true
            })
        );
        assert_eq!(cstate_info(&MsrSpace::new([])), None);
    }

    #[cfg(feature = "acpi")]
    #[test]
    fn processor_objects() {
        let pstates = [decode_pstate(0, 1 << 63 | 8 << 8 | 0x6c).unwrap()];
        let aml = PowerAml::new(&Power {
            pstates: &pstates,
            cstates: Some(CstateInfo {
                io_base: 0x813,
                cc6: true,
            }),
            threads_per_core: 2,
            c2_latency_us: 400,
        });
        let cpu3 = aml.for_cpu(3);
        for name in [b"_PCT", b"_PSS", b"_CST", b"_PSD"] {
            assert!(cpu3.windows(4).any(|w| w == name), "{name:?}");
        }
        // C2 reads 0x814; core 1's domain.
        assert!(cpu3.windows(2).any(|w| w == 0x814u16.to_le_bytes()));
        assert!(cpu3.windows(2).any(|w| w == [0x0b, 0x90]), "400 us");
    }
}
