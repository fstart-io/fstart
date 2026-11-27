// SPDX-License-Identifier: MIT OR Apache-2.0
//! EC host-event routing. Unified wake masks are a command-set feature,
//! independent of the LPC packet protocol (some ECs advertise only the bit).
use super::{Ec, Error, Io, LegacyMask};
use fstart_core::services::Timer;
use zerocopy::little_endian::{U16, U32, U64};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

#[repr(C)]
#[derive(Clone, Copy, FromBytes, Immutable, KnownLayout, Unaligned)]
struct FeaturesResponse {
    flags: [U32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, FromBytes, Immutable, KnownLayout, Unaligned)]
struct CmdVersionsResponse {
    version_mask: U32,
}

#[repr(C)]
#[derive(Clone, Copy, IntoBytes, Immutable)]
struct HostEventMaskRequest {
    action: u8,
    mask_type: u8,
    reserved: [u8; 2],
    mask: U64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventMasks {
    Legacy,
    Unified,
}

impl EventMasks {
    fn probe(
        mut command: impl FnMut(u16, u8, &[u8], &mut [u8]) -> Result<usize, Error>,
    ) -> Result<Self, Error> {
        let mut features = [0; 8];
        match command(0x0d, 0, &[], &mut features) {
            Err(Error::Rejected(1)) => return Ok(Self::Legacy),
            Ok(8) => {
                let parsed =
                    FeaturesResponse::read_from_bytes(&features).map_err(|_| Error::Length)?;
                if parsed.flags[1].get() & 1 == 0 {
                    return Ok(Self::Legacy);
                }
            }
            Ok(_) => return Err(Error::Length),
            Err(e) => return Err(e),
        }
        let mut versions = [0; 4];
        match command(8, 1, U16::new(0xa4).as_bytes(), &mut versions) {
            Ok(4) => {
                let parsed =
                    CmdVersionsResponse::read_from_bytes(&versions).map_err(|_| Error::Length)?;
                if parsed.version_mask.get() & 1 != 0 {
                    Ok(Self::Unified)
                } else {
                    Ok(Self::Legacy)
                }
            }
            Err(Error::Rejected(1 | 3 | 6)) => Ok(Self::Legacy),
            Ok(_) => Err(Error::Length),
            Err(e) => Err(e),
        }
    }

    pub fn detect<I: Io>(ec: &mut Ec<I>, timer: &impl Timer) -> Result<Self, Error> {
        Self::probe(|c, v, i, o| ec.command(c, v, i, o, timer))
    }
    fn unified(mask: u8, events: u32) -> HostEventMaskRequest {
        HostEventMaskRequest {
            action: 1, // SET
            mask_type: mask,
            reserved: [0; 2],
            mask: U64::new(u64::from(events)),
        }
    }
    fn kind(mask: LegacyMask) -> u8 {
        match mask {
            LegacyMask::Sci => 2,
            LegacyMask::Smi => 3,
            LegacyMask::Wake => 5,
        }
    }
    pub fn set<I: Io>(
        self,
        ec: &mut Ec<I>,
        mask: LegacyMask,
        events: u32,
        timer: &impl Timer,
    ) -> Result<(), Error> {
        match self {
            Self::Legacy => ec.set_mask(mask, events, timer),
            Self::Unified => ec
                .command(
                    0xa4,
                    0,
                    Self::unified(Self::kind(mask), events).as_bytes(),
                    &mut [0; 8],
                    timer,
                )
                .map(|_| ()),
        }
    }
    pub fn short_set<I: Io>(
        self,
        ec: &mut Ec<I>,
        mask: LegacyMask,
        events: u32,
        delay: impl FnMut(u32),
    ) -> Result<(), Error> {
        match self {
            Self::Legacy => ec.short_set_mask(mask, events, delay),
            Self::Unified => ec
                .short_command::<16>(
                    0xa4,
                    0,
                    Self::unified(Self::kind(mask), events).as_bytes(),
                    &mut [0; 8],
                    delay,
                )
                .map(|_| ()),
        }
    }
    /// Legacy ECs need the active wake mask at sleep entry; unified ECs
    /// select the already programmed lazy mask themselves.
    pub fn lazy<I: Io>(
        self,
        ec: &mut Ec<I>,
        s5: u32,
        s3: u32,
        timer: &impl Timer,
    ) -> [Result<(), Error>; 2] {
        if self == Self::Legacy {
            return [Ok(()); 2];
        }
        [(8, s5), (7, s3)].map(|(kind, mask)| {
            ec.command(
                0xa4,
                0,
                Self::unified(kind, mask).as_bytes(),
                &mut [0; 8],
                timer,
            )
            .map(|_| ())
        })
    }
}
impl<I: Io> Ec<I> {
    /// Drain up to 32 queued ACPI host events via `QUERY_EVENT` (`0x84`).
    ///
    /// If the first poll times out after submission, completes and discards
    /// only our own pending reply via [`Self::finish_owned`] rather than
    /// issuing a duplicate query or touching OSPM's channel state.
    pub fn drain_events(
        &mut self,
        mut delay: impl FnMut(u32),
        mut on_event: impl FnMut(u8),
    ) -> Result<(), Error> {
        for count in 0..=32 {
            let code = match self.query_event(&mut delay) {
                Err(Error::Timeout) if self.query_pending() => {
                    self.finish_owned(&mut delay)?;
                    return Ok(());
                }
                result => result?,
            };
            if code == 0 {
                return Ok(());
            }
            if count == 32 {
                return Err(Error::Length);
            }
            on_event(code);
        }
        unreachable!()
    }

    /// GET_NEXT_EVENT v0. Unsupported and empty queues are normal on LPC ECs.
    pub fn drain_mkbp(&mut self, mut delay: impl FnMut(u32)) -> Result<(), Error> {
        for _ in 0..=32 {
            match self.short_command::<32>(0x67, 0, &[], &mut [0; 24], &mut delay) {
                Err(Error::Rejected(1 | 9)) => return Ok(()),
                Ok(0) => return Err(Error::Length),
                Ok(_) => (),
                Err(e) => return Err(e),
            }
        }
        Err(Error::Timeout)
    }
}
