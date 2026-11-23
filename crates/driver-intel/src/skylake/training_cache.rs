// SPDX-License-Identifier: MIT OR Apache-2.0
//! Cached DDR4 training from native/save_restore.c.
//!
//! The record holds the MCHBAR images native meminit_save_training() takes
//! right after mc_activate, plus the trained mode registers and per-device MR6
//! codes the fast-boot JEDEC reset replays. Non-training values (topology,
//! timings, frequency) are not stored: the driver re-reads the SPDs and
//! re-resolves the same policy every boot, and `identity` binds the record to
//! those inputs. The platform journal separately binds it to the firmware image.

use super::{registers::MchRegisters, training_state::CalibrationState};
use fstart_core::mmio::MmioRegister;
use sha2::{Digest, Sha256};
use tock_registers::interfaces::{Readable, Writeable};
use zerocopy::byteorder::{LittleEndian, U16, U32};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};

/// Inclusive dword ranges. One table entry per native save_data_control row.
type Ranges = &'static [(u32, u32)];

/// save_data_common: saved once, independent of the SA GV point.
const COMMON: Ranges = &[
    (0x1800, 0x1800), // Ch0 CLK ranks used
    (0x1820, 0x1820), // Ch0 CLK VSSHI comp control
    (0x1900, 0x1900), // Ch1 CLK ranks used
    (0x1920, 0x1920), // Ch1 CLK VSSHI comp control
    (0x2000, 0x2004), // scrambler ch0/1
    (0x3a1c, 0x3a24), // COMP VSSHI, override, VSSHI control
    (0x4030, 0x4038), // Ch0 DFT misc, ECC DFT, PRCNT config
    (0x4040, 0x4044), // Ch0 power-down config, WMM read config
    (0x4068, 0x406c), // Ch0 WDB watermark
    (0x4078, 0x4078), // Ch0 scheduler spare
    (0x4210, 0x4210), // Ch0 DDR MR params
    (0x4230, 0x4234), // Ch0 deswizzle low/high
    (0x4254, 0x4254), // Ch0 MC init state
    (0x42b8, 0x42bc), // Ch0 row hammer LFSR
    (0x42c0, 0x42c0), // Ch0 row hammer control
    (0x43fc, 0x43fc), // Ch0 maintenance spare
    (0x4430, 0x4438), // Ch1 DFT misc, ECC DFT, PRCNT config
    (0x4440, 0x4444), // Ch1 power-down config, WMM read config
    (0x4468, 0x446c), // Ch1 WDB watermark
    (0x4478, 0x4478), // Ch1 scheduler spare
    (0x4610, 0x4610), // Ch1 DDR MR params
    (0x4630, 0x4634), // Ch1 deswizzle low/high
    (0x4654, 0x4654), // Ch1 MC init state
    (0x46b8, 0x46bc), // Ch1 row hammer LFSR
    (0x46c0, 0x46c0), // Ch1 row hammer control
    (0x47fc, 0x47fc), // Ch1 maintenance spare
    (0x5000, 0x5010), // MAD inter-channel, intra ch0/1, DIMM ch0/1
    (0x501c, 0x501c), // MC decode CBIT
    (0x5024, 0x5024), // channel hash
    (0x5030, 0x5034), // MC init state G, MRC revision
    (0x5060, 0x5060), // PM self-refresh config
    (0x5078, 0x5078), // IPC MC arbitration
    (0x50b0, 0x50cc), // bit error recovery source sysaddr 0..3
    (0x5138, 0x5144), // global driver gate, BER range sysaddr
    (0x5880, 0x5880), // PCU DDR PTM control
    (0x5884, 0x5888), // PCU DDR energy scale, RAPL channel power floor
    (0x5890, 0x589c), // PCU DDR warm/hot threshold ch0/1
    (0x58a4, 0x58a4), // PCU DDR voltage
    (0x58d0, 0x58e4), // PCU DDR warm/hot budget ch0/1, RAPL limit
];
/// save_data_common_per_byte: DDRIO data lanes 0..8 (including ECC).
const COMMON_PER_BYTE: Ranges = &[
    (0x08, 0x0c), // RX train, ranks 2/3
    (0x18, 0x1c), // RX per-bit, ranks 2/3
    (0x28, 0x2c), // TX train, ranks 2/3
    (0x38, 0x3c), // TX per-bit, ranks 2/3
    (0x58, 0x5c), // RX offset VDQ, ranks 2/3
    (0x74, 0x78), // Vref control, VSSHI control
    (0x8c, 0x8c), // VTT generation control
];
/// save_data_sagv: one copy per SA GV point; SA GV is off here.
const SAGV: Ranges = &[
    (0x0084, 0x0084), // Ch0 byte 0 Vref adjust 1
    (0x1204, 0x1208), // Ch0 CKE command comp offset, PI coding
    (0x1214, 0x1220), // Ch0 CKE CTL comp offset..ranks used
    (0x1304, 0x1308), // Ch1 CKE command comp offset, PI coding
    (0x1314, 0x1320), // Ch1 CKE CTL comp offset..ranks used
    (0x1404, 0x140c), // Ch0 CMDN comp offset, PI coding, controls
    (0x1504, 0x150c), // Ch1 CMDN comp offset, PI coding, controls
    (0x1808, 0x1810), // Ch0 CLK comp offset, PI coding, controls
    (0x1908, 0x1910), // Ch1 CLK comp offset, PI coding, controls
    (0x1a04, 0x1a0c), // Ch0 CMDS comp offset, PI coding, controls
    (0x1b04, 0x1b0c), // Ch1 CMDS comp offset, PI coding, controls
    (0x1c14, 0x1c20), // Ch0 CTL comp offset..ranks used
    (0x1d14, 0x1d20), // Ch1 CTL comp offset..ranks used
    (0x2008, 0x2008), // DDR misc control 0
    (0x3a14, 0x3a18), // COMP control 0/1
    (0x3a28, 0x3a28), // COMP control 2
    (0x4000, 0x402c), // Ch0 TC pre..scheduler second CBIT
    (0x403c, 0x403c), // Ch0 scheduler PCIT
    (0x4070, 0x4070), // Ch0 TC ODT
    (0x4080, 0x4080), // Ch0 scheduler ODT matrix
    (0x4238, 0x4250), // Ch0 TC RFP..TC MR4 shadow
    (0x4260, 0x4270), // Ch0 PM DIMM energy
    (0x4278, 0x4278), // Ch0 write add delay
    (0x43a0, 0x43cc), // Ch0 DDR4 MR0/MR1 content..MRS FSM control
    (0x4400, 0x442c), // Ch1 TC pre..scheduler second CBIT
    (0x443c, 0x443c), // Ch1 scheduler PCIT
    (0x4470, 0x4470), // Ch1 TC ODT
    (0x4480, 0x4480), // Ch1 scheduler ODT matrix
    (0x4638, 0x4650), // Ch1 TC RFP..TC MR4 shadow
    (0x4660, 0x4670), // Ch1 PM DIMM energy
    (0x4678, 0x4678), // Ch1 write add delay
    (0x47a0, 0x47cc), // Ch1 DDR4 MR0/MR1 content..MRS FSM control
    (0x5130, 0x5134), // scheduler QOS
    (0x59b8, 0x59b8), // PCU MRC ODT power saving
];
/// save_data_sagv_per_byte: lanes 0..7 per point; lane 8 lives in COMMON.
const SAGV_PER_BYTE: Ranges = &[
    (0x00, 0x04), // RX train, ranks 0/1
    (0x10, 0x14), // RX per-bit, ranks 0/1
    (0x20, 0x24), // TX train, ranks 0/1
    (0x30, 0x34), // TX per-bit, ranks 0/1
    (0x48, 0x48), // TX crosstalk
    (0x50, 0x54), // RX offset VDQ, ranks 0/1
    (0x68, 0x70), // data offset comp, data control 1/2
    (0x7c, 0x80), // data offset train, data control 0
];

/// Native REG_COUNT_COMMON / REG_COUNT_SAGV.
pub const COMMON_COUNT: usize = 344;
pub const SAGV_COUNT: usize = 382;

const MAGIC: [u8; 8] = *b"FSKLMRC1";

/// DDRIO per-byte address: byte stride 0x200, channel stride 0x100.
const fn lane(offset: u32, ch: u32, byte: u32) -> u32 {
    offset + ch * 0x100 + byte * 0x200
}

const fn dwords(ranges: Ranges) -> usize {
    let mut i = 0;
    let mut n = 0;
    while i < ranges.len() {
        n += ((ranges[i].1 - ranges[i].0) / 4 + 1) as usize;
        i += 1;
    }
    n
}
const _: () = assert!(
    dwords(COMMON) + 2 * 9 * dwords(COMMON_PER_BYTE) + 2 * dwords(SAGV_PER_BYTE) == COMMON_COUNT
);
const _: () = assert!(dwords(SAGV) + 2 * 8 * dwords(SAGV_PER_BYTE) == SAGV_COUNT);

/// Every MCHBAR offset of one section, in native save/restore order.
fn common_offsets() -> impl Iterator<Item = u32> {
    flat(COMMON)
        .chain(
            flat(COMMON_PER_BYTE)
                .flat_map(|o| (0..2).flat_map(move |ch| (0..9).map(move |byte| lane(o, ch, byte)))),
        )
        .chain(flat(SAGV_PER_BYTE).flat_map(|o| (0..2).map(move |ch| lane(o, ch, 8))))
}
fn sagv_offsets() -> impl Iterator<Item = u32> {
    flat(SAGV).chain(
        flat(SAGV_PER_BYTE)
            .flat_map(|o| (0..2).flat_map(move |ch| (0..8).map(move |byte| lane(o, ch, byte)))),
    )
}
fn flat(ranges: Ranges) -> impl Iterator<Item = u32> {
    ranges
        .iter()
        .flat_map(|&(start, end)| (start..=end).step_by(4))
}

fn register(registers: &MchRegisters, offset: u32) -> MmioRegister<u32> {
    let base = registers as *const MchRegisters as usize;
    // SAFETY: every table offset is a dword inside the whole 32-KiB MCHBAR
    // that `MchRegisters::at` requires to be mapped for this borrow.
    unsafe { MmioRegister::new(base + offset as usize) }
}

/// Inputs a record must match: CPU, fused capabilities and exact SPD bytes.
pub fn identity(cpu_signature: u32, capid: [u32; 3], spd: [[Option<&[u8]>; 2]; 2]) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(MAGIC);
    hash.update(cpu_signature.to_le_bytes());
    for value in capid {
        hash.update(value.to_le_bytes());
    }
    for slot in spd.iter().flatten() {
        match slot {
            Some(bytes) => {
                hash.update([1]);
                hash.update((bytes.len() as u32).to_le_bytes());
                hash.update(bytes);
            }
            None => hash.update([0]),
        }
    }
    hash.finalize().into()
}

#[derive(Clone, FromBytes, IntoBytes, Immutable, KnownLayout)]
#[repr(C)]
pub struct Record {
    magic: [u8; 8],
    identity: [u8; 32],
    mode_registers: [[[U16<LittleEndian>; 7]; 4]; 2],
    pda_enable: u8,
    pda_mr6: [[[u8; 9]; 4]; 2],
    common: [U32<LittleEndian>; COMMON_COUNT],
    sagv: [U32<LittleEndian>; SAGV_COUNT],
}
pub const RECORD_SIZE: usize = core::mem::size_of::<Record>();

impl Record {
    /// Write the native save image into `out` and return its length.
    /// Call exactly where native saves: after mc_activate, before alias check.
    pub fn capture(
        out: &mut [u8],
        registers: &MchRegisters,
        state: &CalibrationState,
        identity: [u8; 32],
    ) -> Option<usize> {
        let record = Self::mut_from_prefix(out).ok()?.0;
        record.magic = MAGIC;
        record.identity = identity;
        for (ch, ranks) in state.saved_mr.rank.iter().enumerate() {
            for (rank, mrs) in ranks.iter().enumerate() {
                for (mr, value) in mrs.iter().enumerate() {
                    record.mode_registers[ch][rank][mr] = (*value).into();
                }
            }
        }
        record.pda_enable = u8::from(state.ddr4_pda_enable);
        record.pda_mr6 = state.ddr4_pda_mr6;
        for (slot, offset) in record.common.iter_mut().zip(common_offsets()) {
            *slot = register(registers, offset).get().into();
        }
        for (slot, offset) in record.sagv.iter_mut().zip(sagv_offsets()) {
            *slot = register(registers, offset).get().into();
        }
        Some(RECORD_SIZE)
    }

    /// The record only if it is this format and was taken for `identity`.
    pub fn parse<'a>(bytes: &'a [u8], identity: &[u8; 32]) -> Option<&'a Self> {
        let record = Self::ref_from_bytes(bytes).ok()?;
        (record.magic == MAGIC && record.identity == *identity && record.pda_enable <= 1)
            .then_some(record)
    }

    /// meminit_restore_non_training_values' mode-register/PDA part. The JEDEC
    /// reset replays these instead of computing first-init images.
    pub fn restore_state(&self, state: &mut CalibrationState) {
        for (ch, ranks) in self.mode_registers.iter().enumerate() {
            for (rank, mrs) in ranks.iter().enumerate() {
                for (mr, value) in mrs.iter().enumerate() {
                    state.saved_mr.rank[ch][rank][mr] = value.get();
                }
            }
        }
        state.ddr4_pda_enable = self.pda_enable != 0;
        state.ddr4_pda_mr6 = self.pda_mr6;
    }

    /// meminit_restore_training's register part (SA GV off: the LOW copy).
    pub fn restore_registers(&self, registers: &MchRegisters) {
        for (value, offset) in self.common.iter().zip(common_offsets()) {
            register(registers, offset).set(value.get());
        }
        for (value, offset) in self.sagv.iter().zip(sagv_offsets()) {
            register(registers, offset).set(value.get());
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use crate::skylake::registers::testing::image;

    #[test]
    fn tables_match_native_counts_and_stay_inside_mchbar() {
        assert_eq!(common_offsets().count(), COMMON_COUNT);
        assert_eq!(sagv_offsets().count(), SAGV_COUNT);
        let mut seen = std::collections::BTreeSet::new();
        for offset in common_offsets().chain(sagv_offsets()) {
            assert!(offset % 4 == 0 && (offset as usize) < super::super::registers::MCHBAR_SIZE);
            assert!(seen.insert(offset), "{offset:#x} saved twice");
        }
    }

    #[test]
    fn capture_restores_registers_and_mode_registers() {
        let source = image();
        for (i, offset) in common_offsets().chain(sagv_offsets()).enumerate() {
            source.set(offset as usize, 0x1000_0000 + i as u32);
        }
        let mut state = CalibrationState::INITIAL;
        state.saved_mr.rank[1][2][6] = 0x0817;
        state.ddr4_pda_enable = true;
        state.ddr4_pda_mr6[1][2][8] = 0x55;
        let key = identity(
            0x506e3,
            [1, 2, 3],
            [[Some(&[1, 2][..]), None], [None, None]],
        );
        let mut out = std::vec![0; RECORD_SIZE + 7];
        assert_eq!(
            Record::capture(&mut out, source.registers(), &state, key),
            Some(RECORD_SIZE)
        );
        let record = Record::parse(&out[..RECORD_SIZE], &key).unwrap();
        assert!(Record::parse(&out[..RECORD_SIZE], &[0; 32]).is_none());
        assert!(Record::parse(&out[..RECORD_SIZE - 1], &key).is_none());

        let target = image();
        record.restore_registers(target.registers());
        let mut restored = CalibrationState::INITIAL;
        record.restore_state(&mut restored);
        assert_eq!(restored.saved_mr, state.saved_mr);
        assert!(restored.ddr4_pda_enable);
        assert_eq!(restored.ddr4_pda_mr6, state.ddr4_pda_mr6);
        for offset in common_offsets().chain(sagv_offsets()) {
            assert_eq!(
                register(target.registers(), offset).get(),
                register(source.registers(), offset).get()
            );
        }
    }

    #[test]
    fn identity_tracks_each_input() {
        let spd = [0u8; 4];
        let base = identity(1, [2, 3, 4], [[Some(&spd[..]), None], [None, None]]);
        assert_ne!(
            base,
            identity(2, [2, 3, 4], [[Some(&spd[..]), None], [None, None]])
        );
        assert_ne!(
            base,
            identity(1, [2, 3, 5], [[Some(&spd[..]), None], [None, None]])
        );
        assert_ne!(
            base,
            identity(1, [2, 3, 4], [[None, Some(&spd[..])], [None, None]])
        );
        assert_ne!(
            base,
            identity(
                1,
                [2, 3, 4],
                [[Some(&[0, 0, 0, 1][..]), None], [None, None]]
            )
        );
    }
}
