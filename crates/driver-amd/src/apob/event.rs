//! APOB 7/6: the ABL event log (PSP_APOB §3.5), the only training
//! diagnostics the x86 side gets.
//!
//! `+0x30` count (u16), events from `+0x34`, 0x10 bytes each
//! `{class, info, data A, data B}` (PspPeiV2Ssp `ffe17252` replays them into
//! the AGESA error log; decompiler-only). Naples and Rome read any instance,
//! Milan's memory modules read the per-socket ones (`socket << 8`).

use zerocopy::little_endian::{U16, U32};
use zerocopy::{FromBytes, Immutable, KnownLayout, Unaligned};

use super::{Apob, ApobError, Entry, group};

const KIND: u32 = 6;

#[derive(FromBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
struct RawEvent {
    class: U32,
    info: U32,
    data_a: U32,
    data_b: U32,
}

/// One event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Event {
    /// Severity class as the ABL reports it.
    pub class: u32,
    /// Event id.
    pub info: u32,
    pub data_a: u32,
    pub data_b: u32,
}

/// An error the ABL found before x86 ran, as the vendors' platform RAS
/// DXE drivers read it from the AGESA error log (the APOB's events) for
/// the BERT and the BMC: Rome AmdPlatformRasSspDxe `0x4a50`, Milan
/// AmdPlatformRasGnDxe, Siena V2.00 AmdPlatformRasRsDxe `0x49cc` (DIMMs),
/// `0x4bb4` (CCD BIST), `0x4ed4` (PMIC).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AblError {
    /// A DIMM the ABL took out of the memory map: its training
    /// ([`id::PMU_TRAINING`]) or memory test ([`id::MEMORY_TEST`])
    /// failed; the event's data as the ABL logged it.
    Dimm {
        event: u32,
        socket: u8,
        channel: u8,
        dimm: u8,
        data_a: u32,
        data_b: u32,
    },
    /// A CCD's BIST failed ([`id::CCD_BIST`]): the event's data A, which
    /// the vendor reports as is, and its data B, both as the ABL logged
    /// them.
    CcdBist { data_a: u32, data_b: u32 },
    /// A DDR5 PMIC error ([`id::PMIC`], the PMIC status the ABL read
    /// in training, or [`id::PMIC_RUNTIME`], the PMIC's error log):
    /// fatal for class 9, the event's data as the ABL logged it.
    Pmic {
        event: u32,
        socket: u8,
        channel: u8,
        dimm: u8,
        fatal: bool,
        data_a: u32,
        data_b: u32,
    },
}

/// The ABL event ids [`AblError`] reads.
pub mod id {
    pub const PMU_TRAINING: u32 = 0x4001;
    pub const MEMORY_TEST: u32 = 0x4003;
    pub const CCD_BIST: u32 = 0x4065;
    pub const PMIC: u32 = 0x406b;
    pub const PMIC_RUNTIME: u32 = 0x406e;
}

/// Where an ABL's memory events name their DIMMs: DIMM 0's bit in data A
/// of a training failure and of a memory test failure (DIMM 1's is the
/// next); socket and channel are data A's bytes 0 and 1 in both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DimmBits {
    pub training: u8,
    pub test: u8,
}

impl DimmBits {
    /// The SSP and GN dies' ABL (Rome AmdPlatformRasSspDxe `0x4a50`, Milan
    /// AmdPlatformRasGnDxe: bits 16 and 17 for both events).
    pub const SSP: Self = Self {
        training: 16,
        test: 16,
    };
    /// The RS die's (Siena V2.00 AmdPlatformRasRsDxe `0x49cc`: a training
    /// failure's DIMMs at bits 20 and 21).
    pub const RS: Self = Self {
        training: 20,
        test: 16,
    };
}

/// The AGESA event class of a fatal error (the vendor's PMIC records
/// are fatal for it).
const CLASS_FATAL: u32 = 9;

impl Event {
    /// The errors this event reports (a memory event can name both DIMMs
    /// of its channel), `bits` locating its DIMMs.
    pub fn errors(self, bits: DimmBits) -> impl Iterator<Item = AblError> {
        let [socket, channel, ..] = self.data_a.to_le_bytes();
        let (info, data_a, data_b) = (self.info, self.data_a, self.data_b);
        let dimms = move |bit: u8| {
            (0..2u8)
                .filter(move |dimm| data_a >> (bit + dimm) & 1 != 0)
                .map(move |dimm| AblError::Dimm {
                    event: info,
                    socket,
                    channel,
                    dimm,
                    data_a,
                    data_b,
                })
        };
        let memory = match self.info {
            id::PMU_TRAINING => Some(dimms(bits.training)),
            id::MEMORY_TEST => Some(dimms(bits.test)),
            _ => None,
        };
        let other = match self.info {
            id::CCD_BIST => Some(AblError::CcdBist {
                data_a: self.data_a,
                data_b: self.data_b,
            }),
            id::PMIC | id::PMIC_RUNTIME => Some(AblError::Pmic {
                event: self.info,
                socket,
                channel,
                dimm: (self.data_a >> 16 & 1) as u8,
                fatal: self.class == CLASS_FATAL,
                data_a: self.data_a,
                data_b: self.data_b,
            }),
            _ => None,
        };
        memory.into_iter().flatten().chain(other)
    }

    /// Names of the ids AGESA acts on (Milan AmdMemTestExPei scans for them
    /// and warm-resets to map out DIMMs, PSP_APOB §3.8).
    pub const fn name(&self) -> Option<&'static str> {
        match self.info {
            0x4003 => Some("memory test failure"),
            0x4067 => Some("memory healing BIST failure"),
            0x4068 => Some("memory address validation failure"),
            _ => None,
        }
    }
}

fn events<'a>(entry: Entry<'a>) -> Result<impl Iterator<Item = Event> + 'a, ApobError> {
    let count: &U16 = entry.at(0x30)?;
    let raw: &[RawEvent] = entry.slice(0x34, count.get().into())?;
    Ok(raw.iter().map(|e| Event {
        class: e.class.get(),
        info: e.info.get(),
        data_a: e.data_a.get(),
        data_b: e.data_b.get(),
    }))
}

/// The events of the 7/6 entry `instance`.
pub fn event_log<'a>(
    apob: &Apob<'a>,
    instance: u32,
) -> Result<impl Iterator<Item = Event> + 'a, ApobError> {
    events(apob.find(group::GENERAL, KIND, instance)?)
}

/// `(instance, event)` over every 7/6 entry; stops at the first malformed
/// entry or log.
pub fn all_events<'a>(
    apob: &Apob<'a>,
) -> impl Iterator<Item = Result<(u32, Event), ApobError>> + 'a {
    apob.find_all(group::GENERAL, KIND).flat_map(|entry| {
        let log = entry.and_then(|e| events(e).map(|ev| (e.instance(), ev)));
        let (ok, err) = match log {
            Ok((instance, ev)) => (Some(ev.map(move |e| Ok((instance, e)))), None),
            Err(e) => (None, Some(Err(e))),
        };
        ok.into_iter().flatten().chain(err)
    })
}

#[cfg(test)]
mod tests {
    use super::super::build::{Body, Builder};
    use super::*;
    use crate::psp::Variant;

    fn log(events: &[[u32; 4]]) -> std::vec::Vec<u8> {
        let raw: std::vec::Vec<u8> = events
            .iter()
            .flatten()
            .flat_map(|w| w.to_le_bytes())
            .collect();
        Body::default()
            .at(0x30, &(events.len() as u16).to_le_bytes())
            .at(0x34, &raw)
            .0
    }

    #[test]
    fn decodes_logs() {
        let data = Builder::new(0x18)
            .entry(7, 6, 0, &log(&[[6, 0x4003, 1, 2]]))
            .entry(7, 6, 0x100, &log(&[[3, 0x1234, 0, 0], [5, 0x4068, 7, 8]]))
            .build();
        let apob = Apob::new(&data, Variant::Gn).unwrap();
        let socket1: std::vec::Vec<_> = event_log(&apob, 0x100).unwrap().collect();
        assert_eq!(
            socket1[1],
            Event {
                class: 5,
                info: 0x4068,
                data_a: 7,
                data_b: 8
            }
        );
        assert_eq!(socket1[1].name(), Some("memory address validation failure"));
        let all: std::vec::Vec<_> = all_events(&apob)
            .map(|e| e.map(|(i, e)| (i, e.info)))
            .collect();
        assert_eq!(
            all,
            [Ok((0, 0x4003)), Ok((0x100, 0x1234)), Ok((0x100, 0x4068))]
        );
    }

    #[test]
    fn the_errors_the_vendors_report() {
        let event = |class, info, data_a, data_b| Event {
            class,
            info,
            data_a,
            data_b,
        };
        let errors = |e: Event, bits| e.errors(bits).collect::<std::vec::Vec<_>>();
        let dimm = |event, socket, channel, dimm, data_a, data_b| AblError::Dimm {
            event,
            socket,
            channel,
            dimm,
            data_a,
            data_b,
        };
        // A training failure of both DIMMs of socket 1's channel 3: bits
        // 20 and 21 on RS, 16 and 17 on SSP; the event's words kept.
        assert_eq!(
            errors(event(6, 0x4001, 0x30_0301, 9), DimmBits::RS),
            [
                dimm(0x4001, 1, 3, 0, 0x30_0301, 9),
                dimm(0x4001, 1, 3, 1, 0x30_0301, 9)
            ]
        );
        assert_eq!(errors(event(6, 0x4001, 0x30_0301, 0), DimmBits::SSP), []);
        assert_eq!(
            errors(event(6, 0x4001, 0x2_0b00, 0), DimmBits::SSP),
            [dimm(0x4001, 0, 11, 1, 0x2_0b00, 0)]
        );
        assert_eq!(
            errors(event(6, 0x4003, 0x1_0200, 0), DimmBits::RS),
            [dimm(0x4003, 0, 2, 0, 0x1_0200, 0)]
        );
        assert_eq!(
            errors(event(9, 0x406b, 0x1_0400, 0x55), DimmBits::RS),
            [AblError::Pmic {
                event: 0x406b,
                socket: 0,
                channel: 4,
                dimm: 1,
                fatal: true,
                data_a: 0x1_0400,
                data_b: 0x55
            }]
        );
        assert_eq!(
            errors(event(6, 0x4065, 7, 0x8000_0001), DimmBits::RS),
            [AblError::CcdBist {
                data_a: 7,
                data_b: 0x8000_0001
            }]
        );
        assert_eq!(errors(event(3, 0x1234, 0xffff_ffff, 0), DimmBits::RS), []);
    }

    #[test]
    fn count_beyond_entry_is_an_error() {
        let body = Body::default().at(0x30, &100u16.to_le_bytes()).len(0x40);
        let data = Builder::new(0x18).entry(7, 6, 0, &body.0).build();
        let apob = Apob::new(&data, Variant::Ssp).unwrap();
        assert!(matches!(
            event_log(&apob, 0),
            Err(ApobError::TooShort { .. })
        ));
        assert!(matches!(
            all_events(&apob).next(),
            Some(Err(ApobError::TooShort { .. }))
        ));
    }
}
