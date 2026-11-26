// SPDX-License-Identifier: MIT OR Apache-2.0
//! The machine-check part of APEI every SoC publishes: the HEST's IA-32
//! machine-check sources and the BERT with the errors the banks kept
//! across the last reset (`ccx::mca::take_boot_errors`).
//!
//! Machine checks are the OS's (its native MCA, its EDAC): the sources
//! are not firmware-first, as in the vendors' HEST (S8040 V2.00: MCE and
//! CMC with only GHES_ASSIST, which Linux ignores). No GHES: an error the
//! OS also reads in its banks would reach its decoding twice, once
//! natively and once through the GHES record
//! (`apei_smca_report_x86_error`). What only firmware sees is the last
//! boot's fatal error, which the reset took from the OS: that is the BERT.
//!
//! The BERT's boot error region is one generic error status block (ACPI
//! 6.5 §18.3.1) with a data entry per error, the worst first: an IA32/x64
//! processor section with the bank's registers as an MSR context, the
//! layout Linux hands to its machine-check decoding ([`mca_entries`]). Its
//! size policy: only complete errors, a context of the SMCA registers a
//! bank has (MCA_STATUS to MCA_MISC1), and a region shorter than the 1 KiB
//! Linux prints and decodes a region up to ([`PRINTABLE`]): the worst
//! errors that fit; the rest are counted ([`boot_error_region`]), logged,
//! and each was reported to the BMC.
//!
//! What the ABL found before x86 ran (its event log in the APOB,
//! `apob::event::AblError`) is in the BERT too, as the vendors' platform
//! RAS DXE drivers record it: a DIMM taken out of the memory map as a
//! platform memory error (map-out event, socket/channel/DIMM as
//! node/card/module), a failed CCD BIST as a processor generic error,
//! a DDR5 PMIC error as a platform memory error of its DIMM, its event
//! in the FRU text. Each is followed by an entry of fstart's own section
//! ([`ABL_EVENT`]) with the ABL's event as it logged it: its id and its
//! data words, which no standard field names (the vendor adds a section
//! of its own for a PMIC error, AMD's GUID FD2EBF1F-..., whose layout is
//! not public). The two are one error: the region has both or neither.

use alloc::vec::Vec;

use fstart_acpi::cper::{self, Entry, Severity};
use fstart_acpi::hest::{self, Bank, MachineCheck};
use fstart_driver_amd::apob::event::AblError;
use fstart_driver_amd::ccx::ras::Record;

/// The HEST's polling interval for corrected errors, as the vendors'.
const POLL_MS: u32 = 5000;

/// The longest boot error region Linux prints and decodes: one shorter
/// than `ACPI_BERT_PRINT_MAX_LEN` (1 KiB, drivers/acpi/apei/bert.c
/// `bert_print_all`); of a longer one it says only that it skipped it.
/// Three MCA entries fit (908 bytes), four do not (1204).
pub const PRINTABLE: usize = 1023;

/// What the machine-check tables are made of.
#[derive(Debug, Clone, Copy)]
pub struct Apei {
    /// The BSP's MCG_CAP: the bank count and the global capabilities.
    pub mcg_cap: u64,
    /// The OS's thresholding limit (`Board::RAS`), 0 for its own.
    pub threshold: u16,
    /// The boot error region: its address and length.
    pub boot_errors: (u64, u32),
}

/// The HEST and the BERT.
pub fn tables(apei: &Apei) -> Vec<Vec<u8>> {
    let count = (apei.mcg_cap & 0xff).min(64) as u8;
    let banks: Vec<Bank> = (0..count).map(Bank::smca).collect();
    let sources = [
        (
            MachineCheck::Exception {
                source_id: 1,
                flags: 0,
                global_capability: apei.mcg_cap,
                global_control: u64::MAX,
            },
            &banks[..],
        ),
        (
            MachineCheck::Corrected {
                source_id: 2,
                flags: 0,
                poll_ms: POLL_MS,
                threshold: apei.threshold.into(),
            },
            &banks[..],
        ),
    ];
    let sources = sources.map(|(kind, banks)| hest::Source::MachineCheck(kind, banks));
    let hest = hest::table(
        fstart_acpi::OEM_ID,
        fstart_acpi::OEM_TABLE_ID,
        fstart_acpi::OEM_REVISION,
        &sources,
    );
    let (address, length) = apei.boot_errors;
    let mut bert = Vec::new();
    fstart_acpi::Aml::to_aml_bytes(
        &fstart_acpi::bert::BERT::new(
            fstart_acpi::OEM_ID,
            fstart_acpi::OEM_TABLE_ID,
            fstart_acpi::OEM_REVISION,
            length,
            address,
        ),
        &mut bert,
    );
    alloc::vec![hest, bert]
}

/// The severity of `record`: fatal with a corrupt processor context,
/// recoverable otherwise if uncorrected.
fn severity(record: &Record) -> Severity {
    if record.fatal() {
        Severity::Fatal
    } else if record.uncorrected() {
        Severity::Recoverable
    } else {
        Severity::Corrected
    }
}

/// fstart's section of an error the ABL logged (an OEM section: UEFI
/// N.2.3 lets a producer define its own), next to the standard section
/// that classifies it, with what that one has no field for. 16 bytes,
/// little endian:
///
/// | Offset | Size | |
/// |---|---|---|
/// | 0 | 4 | the ABL's event id (`apob::event::id`) |
/// | 4 | 4 | its data A, as the ABL logged it |
/// | 8 | 4 | its data B, as the ABL logged it |
/// | 12 | 3 | socket, channel, DIMM it names; FFh where it names none |
/// | 15 | 1 | 0 |
pub const ABL_EVENT: [u8; 16] = cper::guid(
    0x7dea_91eb,
    0xa65e,
    0x4cb6,
    [0xb0, 0x03, 0x6f, 0xb4, 0x40, 0x86, 0xc0, 0x0e],
);

/// The [`ABL_EVENT`] section of `error`.
fn abl_event(error: &AblError) -> Vec<u8> {
    let (event, data_a, data_b, place) = match *error {
        AblError::Dimm {
            event,
            socket,
            channel,
            dimm,
            data_a,
            data_b,
        } => (event, data_a, data_b, [socket, channel, dimm]),
        AblError::CcdBist { data_a, data_b } => (
            fstart_driver_amd::apob::event::id::CCD_BIST,
            data_a,
            data_b,
            [0xff; 3],
        ),
        AblError::Pmic {
            event,
            socket,
            channel,
            dimm,
            data_a,
            data_b,
            ..
        } => (event, data_a, data_b, [socket, channel, dimm]),
    };
    let mut out = Vec::with_capacity(16);
    for word in [event, data_a, data_b] {
        out.extend_from_slice(&word.to_le_bytes());
    }
    out.extend_from_slice(&place);
    out.push(0);
    out
}

/// The FRU text of an ABL error: `S<socket> C<channel> D<dimm> <what>`.
fn fru_text(socket: u8, channel: u8, dimm: u8, what: &str) -> [u8; 20] {
    let mut text = [0u8; 20];
    let mut out = &mut text[..];
    let _ = core::fmt::Write::write_fmt(
        &mut Cursor(&mut out),
        format_args!("S{socket} C{channel} D{dimm} {what}"),
    );
    text
}

struct Cursor<'a, 'b>(&'a mut &'b mut [u8]);

impl core::fmt::Write for Cursor<'_, '_> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let n = s.len().min(self.0.len());
        let (head, tail) = core::mem::take(self.0).split_at_mut(n);
        head.copy_from_slice(&s.as_bytes()[..n]);
        *self.0 = tail;
        Ok(())
    }
}

/// The generic error data entries of an ABL error (`cpuid`: CPUID leaf 1,
/// `brand`: the processor's brand string): its standard section, then
/// its [`ABL_EVENT`].
fn abl_entries(error: &AblError, cpuid: u32, brand: &[u8; 48]) -> Vec<Entry> {
    use fstart_driver_amd::apob::event::id;
    let dimm = |socket: u8, channel: u8, dimm: u8, kind| {
        cper::platform_memory(&cper::MemoryError {
            status: Some(cper::MEMORY_DATA_ERROR),
            node: Some(socket.into()),
            card: Some(channel.into()),
            module: Some(dimm.into()),
            kind,
        })
    };
    let standard = match *error {
        AblError::Dimm {
            event,
            socket,
            channel,
            dimm: module,
            ..
        } => Entry {
            section: cper::PLATFORM_MEMORY,
            severity: Severity::Fatal,
            data: dimm(socket, channel, module, Some(cper::MEMORY_MAP_OUT)),
            fru_text: Some(fru_text(
                socket,
                channel,
                module,
                if event == id::PMU_TRAINING {
                    "train"
                } else {
                    "test"
                },
            )),
        },
        // Its data is not documented: the ABL event section has it, not
        // the processor section's target address.
        AblError::CcdBist { .. } => Entry {
            section: cper::PROCESSOR_GENERIC,
            severity: Severity::Fatal,
            data: cper::processor_generic(cpuid, brand),
            fru_text: None,
        },
        AblError::Pmic {
            event,
            socket,
            channel,
            dimm: module,
            fatal,
            ..
        } => Entry {
            section: cper::PLATFORM_MEMORY,
            severity: if fatal {
                Severity::Fatal
            } else {
                Severity::Recoverable
            },
            data: dimm(socket, channel, module, None),
            fru_text: Some(fru_text(
                socket,
                channel,
                module,
                if event == id::PMIC { "PMIC" } else { "PMIC RT" },
            )),
        },
    };
    let event = Entry {
        section: ABL_EVENT,
        severity: standard.severity,
        data: abl_event(error),
        fru_text: standard.fru_text,
    };
    alloc::vec![standard, event]
}

/// The errors in the ABL's event log of `apob` (every instance), its
/// DIMMs at `bits`.
pub fn abl_errors(
    apob: &fstart_driver_amd::apob::Apob<'_>,
    bits: fstart_driver_amd::apob::event::DimmBits,
) -> Vec<AblError> {
    let mut events = 0u32;
    let errors: Vec<AblError> = fstart_driver_amd::apob::event::all_events(apob)
        .flatten()
        .inspect(|_| events += 1)
        .flat_map(|(_, event)| event.errors(bits))
        .collect();
    fstart_log::info!(
        "ras: the ABL's event log: {} events, {} errors",
        events,
        errors.len() as u32
    );
    errors
}

/// The boot error region of `records` and of the ABL's `abl` (`cpuid`:
/// CPUID leaf 1 of the CPUs, `brand` their brand string) in at most
/// `capacity` bytes: one status block of the errors' data entries, the
/// worst error first, as many complete errors as fit (an ABL error's two
/// entries both or neither); an empty block if there are none. Each error
/// is measured before it goes in: nothing is cut. Returns the region and
/// how many errors it left out.
pub fn boot_error_region(
    records: &[Record],
    abl: &[AblError],
    cpuid: [u32; 4],
    brand: &[u8; 48],
    capacity: usize,
) -> (Vec<u8>, usize) {
    let mut errors: Vec<Vec<Entry>> = records
        .iter()
        .flat_map(|record| mca_entries(record, cpuid))
        .map(|entry| alloc::vec![entry])
        .chain(abl.iter().map(|error| abl_entries(error, cpuid[0], brand)))
        .collect();
    errors.sort_by_key(|entries| entries.iter().map(|e| rank(e.severity)).min());
    within(errors, capacity)
}

/// The status block of the first `errors` (each its entries) that fit in
/// `capacity` bytes, whole, and how many did not.
pub(crate) fn within(errors: Vec<Vec<Entry>>, capacity: usize) -> (Vec<u8>, usize) {
    let mut length = cper::STATUS_HEADER;
    let fit = errors
        .iter()
        .take_while(|entries| {
            length += entries.iter().map(Entry::encoded_len).sum::<usize>();
            length <= capacity
        })
        .count();
    let omitted = errors.len() - fit;
    let entries: Vec<Entry> = errors.into_iter().take(fit).flatten().collect();
    (cper::status_block(&entries), omitted)
}

/// Severities in the order the region lists them: the worst first.
pub(crate) const fn rank(severity: Severity) -> u8 {
    match severity {
        Severity::Fatal => 0,
        Severity::Recoverable => 1,
        Severity::Corrected => 2,
        Severity::Informational => 3,
    }
}

/// The data entries of an MCA bank's errors: its SMCA registers from
/// MCA_STATUS to MCA_MISC1, as Linux's decoding reads them
/// (arch/x86/kernel/cpu/mce/apei.c, `apei_smca_report_x86_error`): the
/// error from the first two, MCA_STATUS and MCA_ADDR, with MCA_MISC0,
/// MCA_IPID and MCA_SYND; MCA_DESTAT and MCA_DEADDR it does not read. So
/// a deferred error that only MCA_DESTAT holds goes in the first two too
/// (the raw registers stay where they are), and a bank with two errors,
/// one in each log, is two entries. A deferred error logged in both
/// (MCA_CONFIG[LogDeferredInMcaStat]) is one.
fn mca_entries(record: &Record, cpuid: [u32; 4]) -> impl Iterator<Item = Entry> {
    errors(record).map(move |error| mca_entry(&error, cpuid))
}

/// The errors of a bank's `record`, one each: its MCA_STATUS and
/// MCA_ADDR are the error's, its raw MCA_DESTAT and MCA_DEADDR stay
/// ([`mca_entries`] says which errors). The BERT, the boot's log and the
/// board's BMC report these, not the bank: a deferred error only MCA_DESTAT
/// holds is not an MCA_STATUS of 0, and a bank with two errors is two.
pub fn errors(record: &Record) -> impl Iterator<Item = Record> {
    use fstart_driver_amd::ccx::ras::McaStatus;
    let in_status = McaStatus::VALID.is_set(record.status);
    let in_destat = McaStatus::VALID.is_set(record.deferred_status);
    let logged_twice = McaStatus::DEFERRED.is_set(record.status)
        && (record.status, record.address) == (record.deferred_status, record.deferred_address);
    let status = in_status.then_some(*record);
    let deferred = (in_destat && !(in_status && logged_twice)).then_some(Record {
        status: record.deferred_status,
        address: record.deferred_address,
        ..*record
    });
    status.into_iter().chain(deferred)
}

/// One error of [`errors`] on its own: what classifies it (its severity,
/// its SEL event) is its MCA_STATUS alone.
pub fn alone(error: &Record) -> Record {
    Record {
        deferred_status: 0,
        deferred_address: 0,
        ..*error
    }
}

/// The data entry of one of [`errors`].
fn mca_entry(error: &Record, cpuid: [u32; 4]) -> Entry {
    let registers = [
        error.status,
        error.address,
        error.misc0,
        error.config,
        error.ipid,
        error.syndrome,
        0,
        error.deferred_status,
        error.deferred_address,
        error.misc1,
    ];
    Entry {
        section: cper::PROCESSOR_IA,
        severity: severity(&alone(error)),
        data: cper::processor_ia(
            error.cpu.into(),
            cpuid,
            0xc000_2001 + 16 * error.bank,
            &registers,
        ),
        fru_text: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fstart_driver_amd::ccx::ras::Kind;

    /// A decoder of the boot error region written from ACPI 6.5 §18.3.2.7.1
    /// (table 18.12, 18.13) and UEFI N.2.4.2, not from the encoder: the
    /// block's header, then every entry's, each section's MSR context.
    struct Decoded {
        block_status: u32,
        severity: u32,
        entries: Vec<(u32, [u8; 16], Vec<u8>)>,
    }

    fn decode(region: &[u8]) -> Decoded {
        let u32_at =
            |bytes: &[u8], at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
        let block_status = u32_at(region, 0);
        let (raw_offset, raw_length) = (u32_at(region, 4), u32_at(region, 8));
        let data_length = u32_at(region, 12) as usize;
        let severity = u32_at(region, 16);
        assert_eq!((raw_offset, raw_length), (0, 0), "no raw data");
        assert_eq!(
            region.len(),
            20 + data_length,
            "one block, nothing after it"
        );
        let mut entries = Vec::new();
        let mut at = 20;
        while at < region.len() {
            let entry = &region[at..];
            let section: [u8; 16] = entry[..16].try_into().unwrap();
            let severity = u32_at(entry, 16);
            let revision = u16::from_le_bytes([entry[20], entry[21]]);
            let length = u32_at(entry, 24) as usize;
            assert!(revision >= 0x300, "a 72-byte header");
            entries.push((severity, section, entry[72..72 + length].to_vec()));
            at += 72 + length;
        }
        assert_eq!(at, region.len(), "the entries fill the block exactly");
        // Block status: entry count in bits 13:4, matching the entries.
        assert_eq!((block_status >> 4 & 0x3ff) as usize, entries.len());
        Decoded {
            block_status,
            severity,
            entries,
        }
    }

    /// The MSR context of an IA32/x64 processor section: its first MSR and
    /// its registers.
    fn context(section: &[u8]) -> (u32, Vec<u64>) {
        let validation = u64::from_le_bytes(section[..8].try_into().unwrap());
        let (infos, contexts) = (validation >> 2 & 0x3f, validation >> 8 & 0x3f);
        assert_eq!((infos, contexts), (1, 1));
        let context = &section[16 + 48 + 64 * infos as usize..];
        assert_eq!(
            u16::from_le_bytes([context[0], context[1]]),
            1,
            "MSR registers"
        );
        let size = u16::from_le_bytes([context[2], context[3]]) as usize;
        let msr = u32::from_le_bytes(context[4..8].try_into().unwrap());
        let registers = context[16..16 + size]
            .chunks(8)
            .map(|r| u64::from_le_bytes(r.try_into().unwrap()))
            .collect();
        (msr, registers)
    }

    #[test]
    fn a_fatal_dram_error_of_the_last_boot() {
        let record = Record {
            kind: Kind::Boot,
            cpu: 0x10,
            bank: 21,
            status: 0xbea0_0000_0000_011b,
            address: 0x1_4000_0000,
            ipid: 0x96_0045_0f00,
            ..Record::EMPTY
        };
        let (region, omitted) =
            boot_error_region(&[record], &[], [0xa00f11, 0, 0, 0], &[0; 48], 16 << 10);
        assert_eq!(omitted, 0);
        // One block of one entry: 20 + 72 + 64 + 64 + 16 + 80 bytes.
        assert_eq!(region.len(), 316);
        let decoded = decode(&region);
        assert_eq!(decoded.severity, Severity::Fatal as u32);
        let section = &decoded.entries[0].2;
        assert_eq!(&section[8..16], &0x10u64.to_le_bytes());
        let (msr, registers) = context(section);
        assert_eq!(msr, 0xc000_2151);
        assert_eq!(
            (registers[0], registers[1], registers[4]),
            (record.status, record.address, record.ipid)
        );
        // No error: an empty block (block status and data length 0),
        // which Linux reads as none.
        let (empty, _) = boot_error_region(&[], &[], [0; 4], &[0; 48], 16 << 10);
        assert_eq!((empty.len(), &empty[..16]), (20, &[0u8; 16][..]));
    }

    /// Several errors in one block, the worst first; a block of three
    /// stays printable by Linux; the region holds complete entries only.
    #[test]
    fn several_errors_are_one_block_worst_first() {
        let error = |bank, status| Record {
            kind: Kind::Boot,
            cpu: 1,
            bank,
            status,
            ..Record::EMPTY
        };
        let corrected = 1 << 63;
        let deferred = 1 << 63 | 1 << 44;
        let fatal = 1 << 63 | 1 << 61 | 1 << 57;
        let records = [
            error(1, corrected),
            error(2, deferred),
            error(3, fatal),
            error(4, deferred),
        ];
        let (region, omitted) = boot_error_region(&records, &[], [0; 4], &[0; 48], 16 << 10);
        assert_eq!(omitted, 0);
        let decoded = decode(&region);
        assert_eq!(decoded.severity, Severity::Fatal as u32);
        // Uncorrected, corrected, several uncorrected; four entries.
        assert_eq!(decoded.block_status & 0xf, 0b0111);
        let banks: Vec<u32> = decoded
            .entries
            .iter()
            .map(|(_, section, data)| {
                assert_eq!(section, &cper::PROCESSOR_IA);
                (context(data).0 - 0xc000_2001) / 16
            })
            .collect();
        assert_eq!(banks, [3, 2, 4, 1]);
        let severities: Vec<u32> = decoded.entries.iter().map(|e| e.0).collect();
        assert_eq!(severities, [1, 0, 0, 2]);
        // Three errors: below Linux's 1 KiB print limit.
        let (three, _) = boot_error_region(&records[..3], &[], [0; 4], &[0; 48], 16 << 10);
        assert!(three.len() < 1024);
        // Room for two complete entries: two, the worst, and two omitted.
        let (two, omitted) = boot_error_region(&records, &[], [0; 4], &[0; 48], 20 + 2 * 296 + 295);
        assert_eq!(omitted, 2);
        let decoded = decode(&two);
        assert_eq!(decoded.entries.len(), 2);
        assert_eq!(decoded.entries[0].0, Severity::Fatal as u32);
    }

    /// The ABL's errors, a standard section and the ABL event section each:
    /// the DIMM it mapped out, a PMIC error with its raw words, a CCD BIST
    /// failure whose data is no address.
    #[test]
    fn the_abls_errors_keep_their_events() {
        let abl = [
            AblError::Dimm {
                event: 0x4001,
                socket: 0,
                channel: 3,
                dimm: 1,
                data_a: 0x2_0300,
                data_b: 0x77,
            },
            AblError::Pmic {
                event: 0x406b,
                socket: 1,
                channel: 11,
                dimm: 0,
                fatal: false,
                data_a: 0x10b01,
                data_b: 0x20,
            },
            AblError::CcdBist {
                data_a: 5,
                data_b: 0xdead_0001,
            },
        ];
        let (region, omitted) =
            boot_error_region(&[], &abl, [0xa10f11, 0, 0, 0], &[b'x'; 48], 16 << 10);
        assert_eq!(omitted, 0);
        let decoded = decode(&region);
        let sections: Vec<[u8; 16]> = decoded.entries.iter().map(|e| e.1).collect();
        // The fatal ones first: the DIMM, the BIST; then the PMIC's.
        assert_eq!(
            sections,
            [
                cper::PLATFORM_MEMORY,
                ABL_EVENT,
                cper::PROCESSOR_GENERIC,
                ABL_EVENT,
                cper::PLATFORM_MEMORY,
                ABL_EVENT
            ]
        );
        let entry = |n: usize| &decoded.entries[n].2;
        // Map-out event of socket 0, channel 3, DIMM 1.
        assert_eq!(&entry(0)[32..38], &[0, 0, 3, 0, 1, 0]);
        assert_eq!(entry(0)[72], 15);
        // The map-out's event as the ABL logged it, both words.
        assert_eq!(
            &entry(1)[..],
            &[1, 0x40, 0, 0, 0, 3, 2, 0, 0x77, 0, 0, 0, 0, 3, 1, 0]
        );
        // The BIST's data is in its event, not a target address.
        let generic = entry(2);
        assert_eq!(generic[1] & 2, 0, "no target address valid");
        assert_eq!(&generic[160..168], &[0; 8]);
        assert_eq!(&generic[16..20], &0xa10f11u32.to_le_bytes());
        // The BIST's event with both its words.
        assert_eq!(
            &entry(3)[..12],
            &[0x65, 0x40, 0, 0, 5, 0, 0, 0, 1, 0, 0xad, 0xde]
        );
        assert_eq!(&entry(3)[12..], &[0xff, 0xff, 0xff, 0]);
        // The PMIC's event with both raw words; recoverable.
        assert_eq!(decoded.entries[4].0, Severity::Recoverable as u32);
        assert_eq!(
            &entry(5)[..12],
            &[0x6b, 0x40, 0, 0, 1, 0x0b, 1, 0, 0x20, 0, 0, 0]
        );
        assert_eq!(&entry(5)[12..15], &[1, 11, 0]);
    }

    /// MCA errors and ABL errors in a region one byte too small for all of
    /// them: the last (least severe) error is left out whole, the region is
    /// one complete block, and the count says so.
    #[test]
    fn a_full_region_leaves_whole_errors_out() {
        let mca = |bank, status| Record {
            kind: Kind::Boot,
            cpu: 1,
            bank,
            status,
            ..Record::EMPTY
        };
        let records = [mca(1, 1 << 63 | 1 << 61 | 1 << 57), mca(2, 1 << 63)];
        let abl = [
            AblError::Pmic {
                event: 0x406e,
                socket: 0,
                channel: 1,
                dimm: 1,
                fatal: false,
                data_a: 0x10100,
                data_b: 0x8,
            },
            AblError::CcdBist {
                data_a: 9,
                data_b: 0,
            },
        ];
        let (all, omitted) = boot_error_region(&records, &abl, [0; 4], &[0; 48], 16 << 10);
        assert_eq!(omitted, 0);
        let (region, omitted) = boot_error_region(&records, &abl, [0; 4], &[0; 48], all.len() - 1);
        assert_eq!(omitted, 1);
        let decoded = decode(&region);
        // The fatal MCA error, the BIST (two entries), the PMIC's (two);
        // the corrected MCA error left out.
        assert_eq!(decoded.entries.len(), 5);
        assert_eq!(region.len(), all.len() - 296);
        // Just room for the fatal ones: the PMIC's both entries go.
        let fatal = 20 + 296 + (72 + 192) + (72 + 16);
        let (region, omitted) = boot_error_region(&records, &abl, [0; 4], &[0; 48], fatal + 100);
        assert_eq!((omitted, decode(&region).entries.len()), (2, 3));
    }

    /// What Linux's SMCA decoding of a processor section makes of it
    /// (`apei_smca_report_x86_error`): the context must start at an
    /// MCA_STATUS (bank bits ignored) and hold at least the registers to
    /// MCA_SYND; the bank from its address, the error from the first
    /// registers.
    fn linux_mce(section: &[u8]) -> (u32, u64, u64, u64) {
        let (msr, registers) = context(section);
        assert_eq!(msr & 0xc000_2001, 0xc000_2001, "an SMCA MCA_STATUS");
        assert!(registers.len() >= 6);
        let bank = msr >> 4 & 0xff;
        (bank, registers[0], registers[1], registers[4])
    }

    /// A deferred error only MCA_DESTAT holds reaches Linux's decoding as
    /// the error (MCA_STATUS slot: valid, deferred; its address), the raw
    /// registers kept. A bank with an uncorrected error in MCA_STATUS and
    /// another, deferred, in MCA_DESTAT is two entries, both decoded; one
    /// deferred error logged in both is one.
    #[test]
    fn deferred_errors_reach_the_os_as_linux_decodes_them() {
        let region =
            |records: &[Record]| boot_error_region(records, &[], [0; 4], &[0; 48], PRINTABLE).0;
        let deferred = 1 << 63 | 1 << 58 | 1 << 44 | 0x9b;
        let destat_only = Record {
            kind: Kind::Boot,
            cpu: 2,
            bank: 21,
            deferred_status: deferred,
            deferred_address: 0x2000,
            ipid: 0x96 << 32,
            ..Record::EMPTY
        };
        let decoded = decode(&region(&[destat_only]));
        assert_eq!(decoded.entries.len(), 1);
        assert_eq!(decoded.entries[0].0, Severity::Recoverable as u32);
        let section = &decoded.entries[0].2;
        assert_eq!(linux_mce(section), (21, deferred, 0x2000, 0x96 << 32));
        let (_, raw) = context(section);
        assert_eq!((raw[7], raw[8]), (deferred, 0x2000));

        let uc = 1 << 63 | 1 << 61 | 1 << 58 | 1 << 57 | 0x11b;
        let two = Record {
            status: uc,
            address: 0x1000,
            ..destat_only
        };
        let errors: Vec<_> = decode(&region(&[two]))
            .entries
            .iter()
            .map(|(severity, _, section)| (*severity, linux_mce(section)))
            .collect();
        assert_eq!(
            errors,
            [
                (Severity::Fatal as u32, (21, uc, 0x1000, 0x96 << 32)),
                (
                    Severity::Recoverable as u32,
                    (21, deferred, 0x2000, 0x96 << 32)
                ),
            ]
        );

        let twice = Record {
            status: deferred,
            address: 0x2000,
            ..destat_only
        };
        assert_eq!(decode(&region(&[twice])).entries.len(), 1);
    }

    /// Four MCA errors: the region Linux prints holds the worst three,
    /// under 1 KiB, and counts the fourth.
    #[test]
    fn four_errors_leave_the_least_out_of_a_printable_region() {
        let error = |bank, status| Record {
            kind: Kind::Boot,
            cpu: 1,
            bank,
            status,
            ..Record::EMPTY
        };
        let records = [
            error(1, 1 << 63),
            error(2, 1 << 63 | 1 << 44),
            error(3, 1 << 63 | 1 << 61 | 1 << 57),
            error(4, 1 << 63 | 1 << 44),
        ];
        let (region, omitted) = boot_error_region(&records, &[], [0; 4], &[0; 48], PRINTABLE);
        assert!(region.len() <= PRINTABLE);
        assert_eq!(omitted, 1);
        let banks: Vec<u32> = decode(&region)
            .entries
            .iter()
            .map(|(_, _, section)| linux_mce(section).0)
            .collect();
        assert_eq!(banks, [3, 2, 4], "the corrected one left out");
    }

    /// The log and the BMC get the errors the BERT has: a deferred error
    /// only MCA_DESTAT holds as its own status and address, a bank with an
    /// error in each log as two, each classified by its own status (the
    /// SEL: the UE a memory UE, the deferred one too, not a CE).
    #[test]
    fn the_log_and_the_bmc_get_the_berts_errors() {
        use crate::smm::ras::{Event, sel};
        use zerocopy::IntoBytes;
        let deferred = 1 << 63 | 1 << 58 | 1 << 44 | 0x9b;
        let uc = 1 << 63 | 1 << 61 | 1 << 58 | 1 << 57 | 0x11b;
        let bank = Record {
            kind: Kind::Boot,
            cpu: 2,
            bank: 21,
            status: uc,
            address: 0x1000,
            deferred_status: deferred,
            deferred_address: 0x2000,
            ipid: 0x96 << 32,
            ..Record::EMPTY
        };
        let found: Vec<_> = errors(&bank).map(|e| (e.status, e.address)).collect();
        assert_eq!(found, [(uc, 0x1000), (deferred, 0x2000)]);
        let destat_only = Record {
            status: 0,
            address: 0,
            ..bank
        };
        let found: Vec<_> = errors(&destat_only).collect();
        assert_eq!(
            (found.len(), found[0].status, found[0].address),
            (1, deferred, 0x2000)
        );
        let event = sel(&Event::Mca(&alone(&found[0])));
        assert_eq!(&event.as_bytes()[10..], &[0x0c, 0xff, 0x6f, 0xa1, 2, 21]);
        // A corrected error in MCA_STATUS next to the deferred one: the
        // corrected one is a CE on its own.
        let ce = Record {
            status: 1 << 63 | 0x11b,
            ..bank
        };
        let first = errors(&ce).next().unwrap();
        assert!(!alone(&first).uncorrected());
        assert!(errors(&ce).nth(1).unwrap().uncorrected());
    }

    /// MCA and ABL errors in the printable region: whole errors, the worst
    /// first, an ABL error's two entries together; what does not fit is
    /// counted.
    #[test]
    fn mca_and_abl_errors_share_the_printable_region() {
        let mca = |bank, status| Record {
            kind: Kind::Boot,
            cpu: 1,
            bank,
            status,
            ..Record::EMPTY
        };
        let records = [mca(1, 1 << 63 | 1 << 61 | 1 << 57), mca(2, 1 << 63)];
        let abl = [
            AblError::Pmic {
                event: 0x406e,
                socket: 0,
                channel: 1,
                dimm: 1,
                fatal: false,
                data_a: 0x10100,
                data_b: 0x8,
            },
            AblError::CcdBist {
                data_a: 9,
                data_b: 3,
            },
        ];
        let (region, omitted) = boot_error_region(&records, &abl, [0; 4], &[0; 48], PRINTABLE);
        assert!(region.len() <= PRINTABLE);
        assert_eq!(omitted, 1);
        let sections: Vec<[u8; 16]> = decode(&region).entries.iter().map(|e| e.1).collect();
        assert_eq!(
            sections,
            [
                cper::PROCESSOR_IA,
                cper::PROCESSOR_GENERIC,
                ABL_EVENT,
                cper::PLATFORM_MEMORY,
                ABL_EVENT
            ],
            "the corrected MCA error left out"
        );
    }

    #[test]
    fn hest_and_bert() {
        let tables = tables(&Apei {
            mcg_cap: 0x120,
            threshold: 10,
            boot_errors: (0x7500_0000, 20),
        });
        assert_eq!(&tables[0][..4], b"HEST");
        assert_eq!(tables[0].len(), 40 + 2 * 40 + 8 + 2 * 28 * 32);
        assert_eq!(&tables[1][..4], b"BERT");
        assert_eq!(&tables[1][36..40], &20u32.to_le_bytes());
        assert_eq!(&tables[1][40..48], &0x7500_0000u64.to_le_bytes());
    }
}
