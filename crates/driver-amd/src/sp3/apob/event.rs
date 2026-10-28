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

impl Event {
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
    use crate::sp3::soc::Generation;

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
        let apob = Apob::new(&data, Generation::Milan).unwrap();
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
    fn count_beyond_entry_is_an_error() {
        let body = Body::default().at(0x30, &100u16.to_le_bytes()).len(0x40);
        let data = Builder::new(0x18).entry(7, 6, 0, &body.0).build();
        let apob = Apob::new(&data, Generation::Rome).unwrap();
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
