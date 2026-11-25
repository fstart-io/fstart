// SPDX-License-Identifier: MIT OR Apache-2.0
//! Reusable ChromeEC boot/resume event initialization and SMM ownership handoff.
//!
//! Boards supply only their [`EventPolicy`], SMI GPI bitmask, EC discovery
//! callback, and sleep-rail hook; the event-routing protocol, channel draining,
//! one-shot APM handoff, and poisoned-channel cancellation live here once.

use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use fstart_core::services::Timer;

use crate::{Ec, Error, EventMasks, HostEvent, Io, LegacyMask};

/// Board-private APM commands consumed at most once by [`SmmState`] after
/// ramstage installs the permanent SMM handler. They carry a closed enum,
/// never a pointer or an OS-selected mask.
pub const APM_EC_OFF: u8 = 0x78;
pub const APM_EC_LEGACY: u8 = 0x79;
pub const APM_EC_UNIFIED: u8 = 0x7a;

/// Encode the ramstage-detected [`EventMasks`] mode for the one-shot APM handoff.
#[must_use]
pub const fn apm_handoff_cmd(policy: Option<EventMasks>) -> u8 {
    match policy {
        None => APM_EC_OFF,
        Some(EventMasks::Legacy) => APM_EC_LEGACY,
        Some(EventMasks::Unified) => APM_EC_UNIFIED,
    }
}

/// Not a BSP command: entered only by [`SmmState`] after its own submitted
/// request failed both completion deadlines.
const EC_OUTSTANDING: u8 = 0xff;

/// Board-selected ChromeEC host-event masks and S4/S5 USB charging port count.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EventPolicy {
    pub sci: u32,
    pub smi: u32,
    pub wake_s3: u32,
    pub wake_s5: u32,
    /// Number of USB charging ports (`0..usb_charge_ports`) disabled on S4/S5 entry.
    pub usb_charge_ports: u8,
}

/// Initialize ChromeEC host-event masks during BSP ramstage (cold boot or S3 resume).
///
/// Settled operations are reported individually via `report` so a later failure
/// does not obscure which mask actually took effect. Always finishes any owned
/// reply before returning.
pub fn init_events<I: Io>(
    ec: &mut Ec<I>,
    policy: &EventPolicy,
    resume: bool,
    timer: &impl Timer,
    mut report: impl FnMut(&'static str, Result<(), Error>),
) -> Result<EventMasks, Error> {
    use LegacyMask::{Sci, Smi, Wake};

    let masks = EventMasks::detect(ec, timer)?;
    let mut failed = None;
    let mut record = |op: &'static str, result: Result<(), Error>| {
        if let Err(e) = result {
            failed = Some(e);
        }
        report(op, result);
    };

    record("SMI=0", masks.set(ec, Smi, 0, timer));
    if resume {
        record("SCI=runtime", masks.set(ec, Sci, policy.sci, timer));
    } else {
        let [s5, s3] = masks.lazy(ec, policy.wake_s5, policy.wake_s3, timer);
        if masks == EventMasks::Unified {
            record("lazy S5", s5);
            record("lazy S3", s3);
        }
    }
    record("wake=0", masks.set(ec, Wake, 0, timer));
    if resume && let Err(e) = ec.drain_events(|n| timer.delay_us(n.into()), |_| {}) {
        report("resume QUERY", Err(e));
        // No event to dispatch. Do not leave a late firmware reply in OBF
        // for the OS at the end of resume.
        if let Err(e) = ec.finish_owned(|n| timer.delay_us(n.into())) {
            failed = Some(e);
        }
    }
    // A timeout in a host mask command likewise does not authorize replay.
    ec.finish_owned(|n| timer.delay_us(n.into()))?;
    failed.map_or(Ok(masks), Err)
}

/// Coincident SMI event sources processed within a single SMM entry.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SmiSources {
    pub legacy_gpi: bool,
    pub switch: Option<bool>,
    pub sleep: Option<u8>,
}

/// Execute ChromeEC event-channel work for one SMM entry (legacy GPI drain,
/// ACPI enable/disable mask transfer, and sleep-entry preparation).
#[allow(clippy::too_many_arguments)]
pub fn handle_events<I: Io>(
    ec: &mut Ec<I>,
    masks: EventMasks,
    policy: &EventPolicy,
    sources: SmiSources,
    mut delay: impl FnMut(u32),
    mut lid_close: impl FnMut(),
    mut sleep_rails: impl FnMut(),
    mut report: impl FnMut(u8, Error),
) -> Result<(), Error> {
    use LegacyMask::{Sci, Smi, Wake};

    // Process the interrupted owner's GPI before handing the queue over.
    let mut checked = |op, r: Result<(), Error>| r.inspect_err(|&e| report(op, e));
    let mut error = None;
    if sources.legacy_gpi
        && let Err(e) = checked(
            0x84,
            ec.drain_events(&mut delay, |code| {
                if code == HostEvent::LidClosed as u8 {
                    lid_close();
                }
            }),
        )
    {
        error = Some(e);
    }
    if let Some(acpi) = sources.switch {
        let (old, new, events) = if acpi {
            (Smi, Sci, policy.sci)
        } else {
            (Sci, Smi, policy.smi)
        };
        let transfer = checked(old as u8, masks.short_set(ec, old, 0, &mut delay))
            .and_then(|()| checked(0x84, ec.drain_events(&mut delay, |_| {})))
            .and_then(|()| checked(0x67, ec.drain_mkbp(&mut delay)))
            .and_then(|()| checked(new as u8, masks.short_set(ec, new, events, &mut delay)));
        if let Err(e) = transfer {
            error = Some(e);
        }
    }
    if let Some(state) = sources.sleep {
        // Prepare before any irreversible board rail change. Clear both
        // runtime masks even when the interrupted route was already disabled.
        let smi = checked(Smi as u8, masks.short_set(ec, Smi, 0, &mut delay));
        let sci = checked(Sci as u8, masks.short_set(ec, Sci, 0, &mut delay));
        smi.and(sci)?;
        checked(0x84, ec.drain_events(&mut delay, |_| {}))?;
        checked(0x67, ec.drain_mkbp(&mut delay))?;
        if state >= 4 {
            for port in 0..policy.usb_charge_ports {
                if let Err(e) =
                    checked(0xf4 | port, ec.short_disable_usb_charging(port, &mut delay))
                    && ec.is_poisoned()
                {
                    return Err(e);
                }
            }
        }
        if matches!(state, 3..=5) {
            sleep_rails();
            if masks == EventMasks::Legacy {
                checked(
                    Wake as u8,
                    masks.short_set(
                        ec,
                        Wake,
                        if state == 3 {
                            policy.wake_s3
                        } else {
                            policy.wake_s5
                        },
                        &mut delay,
                    ),
                )?;
            }
        }
    }
    error.map_or(Ok(()), Err)
}

/// Inputs extracted from the chipset SMI dispatcher for [`SmmState::dispatch`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SmiInput {
    pub apm: Option<u8>,
    pub acpi_enabled: bool,
    pub requested_acpi_mode: Option<bool>,
    pub gpi: u32,
    pub sleep: Option<u8>,
}

/// Returned by [`SmmState::dispatch`] when firmware still has an unconsumed
/// ChromeEC reply pending and must cancel the requested ownership transfer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HandoffCancelled;

/// Permanent SMM-resident ChromeEC handoff state machine.
///
/// The BSP supplies one APM byte (`APM_EC_OFF` / `APM_EC_LEGACY` /
/// `APM_EC_UNIFIED`) immediately after installing SMM, before running any
/// payload. Later APM writes cannot re-arm a disabled or poisoned policy.
pub struct SmmState {
    policy: AtomicU8,
    smi_routed: AtomicBool,
}

impl Default for SmmState {
    fn default() -> Self {
        Self::new()
    }
}

impl SmmState {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            policy: AtomicU8::new(0),
            smi_routed: AtomicBool::new(false),
        }
    }

    #[must_use]
    pub fn smi_routed(&self) -> bool {
        self.smi_routed.load(Ordering::Relaxed)
    }

    /// Handle one SMI for a board using ChromeEC LPC handoff.
    #[allow(clippy::too_many_arguments)]
    pub fn dispatch<I: Io>(
        &self,
        policy: &EventPolicy,
        smi_gpi_mask: u32,
        input: SmiInput,
        discover_ec: impl FnOnce() -> Result<Ec<I>, Error>,
        mut delay_us: impl FnMut(u32),
        mut lid_close: impl FnMut(),
        mut sleep_rails: impl FnMut(),
        mut post: impl FnMut(u8),
    ) -> Result<(), HandoffCancelled> {
        let initial_handoff =
            if let Some(cmd @ (APM_EC_OFF | APM_EC_LEGACY | APM_EC_UNIFIED)) = input.apm {
                // One installation-time handoff; later APM writes cannot re-arm
                // a feature that ramstage or an ambiguous EC response disabled.
                self.policy
                    .compare_exchange(0, cmd, Ordering::Relaxed, Ordering::Relaxed)
                    .is_ok()
            } else {
                false
            };
        let masks = match self.policy.load(Ordering::Relaxed) {
            APM_EC_LEGACY => EventMasks::Legacy,
            APM_EC_UNIFIED => EventMasks::Unified,
            EC_OUTSTANDING if input.requested_acpi_mode == Some(true) && !input.acpi_enabled => {
                return Err(HandoffCancelled);
            }
            _ => return Ok(()),
        };
        let gpi = self.smi_routed.load(Ordering::Relaxed)
            && !input.acpi_enabled
            && input.gpi & smi_gpi_mask != 0;
        // On cold boot, SMM relocation clears SCI_EN directly without issuing
        // APM_CNT_ACPI_DISABLE. Arm legacy EC lid-close SMI on the one-time
        // installation handoff when SCI_EN is clear (cold boot / payload mode);
        // on S3 resume SCI_EN is already set so keep SCI active.
        let mode = input
            .requested_acpi_mode
            .or_else(|| (initial_handoff && !input.acpi_enabled).then_some(false));
        if !gpi && mode.is_none() && input.sleep.is_none() {
            return Ok(());
        }
        let mut outstanding = false;
        let result = discover_ec().and_then(|mut ec| {
            let result = handle_events(
                &mut ec,
                masks,
                policy,
                SmiSources {
                    legacy_gpi: gpi,
                    switch: mode,
                    sleep: input.sleep,
                },
                &mut delay_us,
                &mut lid_close,
                &mut sleep_rails,
                |op, e| {
                    post(0xf6);
                    post(op);
                    post(match e {
                        Error::Rejected(status) => status as u8,
                        Error::Unavailable => 0xe0,
                        Error::Timeout => 0xe1,
                        Error::Poisoned => 0xe2,
                        Error::Checksum => 0xe3,
                        Error::Header => 0xe4,
                        Error::Length => 0xe5,
                        Error::UnsupportedProtocol => 0xe6,
                    });
                },
            );
            // Never read OSPM's pending result on a pre-submission rejection.
            // Conversely, consume our own late result before the handoff, even
            // if the original command timed out.
            if ec.finish_owned(&mut delay_us).is_err() {
                outstanding = true;
                post(0xf3);
                return Err(Error::Timeout);
            }
            result
        });
        if result.is_err() {
            self.policy.store(
                if outstanding {
                    EC_OUTSTANDING
                } else {
                    APM_EC_OFF
                },
                Ordering::Relaxed,
            );
            post(0xf2); // EC work disabled, not a reset/veto
        }
        if result.is_err() || input.sleep.is_some() {
            self.smi_routed.store(false, Ordering::Relaxed);
        } else if let Some(acpi) = mode {
            self.smi_routed.store(!acpi, Ordering::Relaxed);
        }
        // Optional EC tuning never requests a reset; refuse only an ownership
        // transfer while firmware output is still pending in the channel.
        if outstanding {
            Err(HandoffCancelled)
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use crate::HostEvent::*;
    use std::{cell::RefCell, vec::Vec};

    const TEST_POLICY: EventPolicy = EventPolicy {
        sci: LidClosed.mask()
            | LidOpen.mask()
            | AcConnected.mask()
            | AcDisconnected.mask()
            | BatteryLow.mask()
            | BatteryCritical.mask()
            | Battery.mask()
            | ThermalThreshold.mask(),
        smi: LidClosed.mask(),
        wake_s5: LidOpen.mask() | PowerButton.mask(),
        wake_s3: LidOpen.mask() | PowerButton.mask() | KeyPressed.mask(),
        usb_charge_ports: 2,
    };

    struct Clock;
    impl Timer for Clock {
        fn timestamp_us(&self) -> u64 {
            0
        }
        fn delay_us(&self, _: u64) {}
    }

    struct Ports<'a> {
        pkt: [u8; 24],
        query: bool,
        left: u8,
        reject: u16,
        result: u8,
        trace: &'a RefCell<Vec<(u16, u32)>>,
    }

    impl<'a> Ports<'a> {
        fn new(trace: &'a RefCell<Vec<(u16, u32)>>) -> Self {
            Self {
                pkt: [0; 24],
                query: false,
                left: 0,
                reject: 0,
                result: 0,
                trace,
            }
        }
    }

    impl Io for Ports<'_> {
        fn read8(&mut self, p: u16) -> u8 {
            match p {
                0x920 => b'E',
                0x921 => b'C',
                0x927 => 2,
                0x204 => 0,
                0x200 => self.result,
                0x66 => self.query.into(),
                0x62 => {
                    self.query = false;
                    if self.left > 0 {
                        self.left -= 1;
                        1
                    } else {
                        0
                    }
                }
                0x800..=0x817 => self.pkt[usize::from(p - 0x800)],
                _ => unreachable!(),
            }
        }
        fn write8(&mut self, p: u16, b: u8) {
            match p {
                0x800..=0x817 => self.pkt[usize::from(p - 0x800)] = b,
                0x66 => {
                    assert!(!self.query);
                    self.query = true;
                    self.trace.borrow_mut().push((0x84, 0));
                }
                0x204 => {
                    let c = u16::from_le_bytes([self.pkt[2], self.pkt[3]]);
                    let n = usize::from(self.pkt[6]);
                    let value = self.pkt[8..8 + n]
                        .iter()
                        .take(4)
                        .enumerate()
                        .fold(0, |a, (i, &v)| a | u32::from(v) << (8 * i));
                    self.trace.borrow_mut().push((c, value));
                    self.result = if c == 0x67 {
                        9
                    } else {
                        u8::from(c == self.reject || c == 0xd)
                    };
                    self.pkt = [0; 24];
                    self.pkt[0] = 3;
                    self.pkt[1] = 253;
                }
                _ => unreachable!(),
            }
        }
    }

    #[test]
    fn init_cold_resume_and_partial_failure() {
        for resume in [false, true] {
            for reject in [0, 0x8a] {
                let trace = RefCell::new(Vec::new());
                let mut ports = Ports::new(&trace);
                ports.reject = reject;
                let mut ec = Ec::discover(ports).unwrap();
                assert_eq!(
                    init_events(&mut ec, &TEST_POLICY, resume, &Clock, |_, _| {}).is_ok(),
                    reject == 0
                );
                let mut expected = std::vec![(0xd, 0), (0x8a, 0)];
                if resume {
                    expected.push((0x8b, 0x1fb));
                }
                expected.push((0x8e, 0));
                if resume {
                    expected.push((0x84, 0));
                }
                assert_eq!(trace.into_inner(), expected);
            }
        }
    }

    #[test]
    fn thirty_two_queries_and_the_terminating_zero() {
        let trace = RefCell::default();
        let mut ports = Ports::new(&trace);
        ports.left = 32;
        let mut ec = Ec::discover(ports).unwrap();
        ec.drain_events(|_| {}, |_| {}).unwrap();
        assert_eq!(trace.borrow().len(), 33);
        let mut ports = Ports::new(&trace);
        ports.left = 33;
        let mut seen = 0;
        let mut ec = Ec::discover(ports).unwrap();
        assert_eq!(ec.drain_events(|_| {}, |_| seen += 1), Err(Error::Length));
        assert_eq!(seen, 32);
    }

    #[test]
    fn coincident_gpi_apmc_sleep_and_charge_rail_wake_order() {
        for state in [0, 1, 3, 4, 5] {
            let trace = RefCell::default();
            let mut ports = Ports::new(&trace);
            ports.left = 1;
            let mut ec = Ec::discover(ports).unwrap();
            handle_events(
                &mut ec,
                EventMasks::Legacy,
                &TEST_POLICY,
                SmiSources {
                    legacy_gpi: true,
                    switch: Some(false),
                    sleep: Some(state),
                },
                |_| {},
                || trace.borrow_mut().push((0x100, 0)),
                || trace.borrow_mut().push((0x101, 0)),
                |_, _| unreachable!(),
            )
            .unwrap();
            let mut want = std::vec![
                (0x84, 0),
                (0x100, 0),
                (0x84, 0),
                (0x8b, 0),
                (0x84, 0),
                (0x67, 0),
                (0x8a, 1),
                (0x8a, 0),
                (0x8b, 0),
                (0x84, 0),
                (0x67, 0),
            ];
            if state >= 4 {
                want.extend([(0x30, 0), (0x30, 1)]);
            }
            if state >= 3 {
                want.extend([(0x101, 0), (0x8e, if state == 3 { 0x1006 } else { 6 })]);
            }
            assert_eq!(trace.into_inner(), want);
        }
    }

    #[test]
    fn rejected_charging_does_not_veto_sleep_or_disable_routing() {
        let trace = RefCell::default();
        let mut ports = Ports::new(&trace);
        ports.reject = 0x30;
        let mut ec = Ec::discover(ports).unwrap();
        let mut rejected = Vec::new();
        handle_events(
            &mut ec,
            EventMasks::Legacy,
            &TEST_POLICY,
            SmiSources {
                legacy_gpi: false,
                switch: None,
                sleep: Some(5),
            },
            |_| {},
            || unreachable!(),
            || trace.borrow_mut().push((0x101, 0)),
            |op, e| rejected.push((op, e)),
        )
        .unwrap();
        assert_eq!(
            rejected,
            [(0xf4, Error::Rejected(1)), (0xf5, Error::Rejected(1))]
        );
        assert!(
            trace
                .borrow()
                .ends_with(&[(0x30, 0), (0x30, 1), (0x101, 0), (0x8e, 6)])
        );
    }

    #[test]
    fn failed_mask_skips_rails_but_both_masks_attempted() {
        let trace = RefCell::default();
        let mut ports = Ports::new(&trace);
        ports.reject = 0x8a;
        let mut ec = Ec::discover(ports).unwrap();
        assert!(
            handle_events(
                &mut ec,
                EventMasks::Legacy,
                &TEST_POLICY,
                SmiSources {
                    legacy_gpi: false,
                    switch: None,
                    sleep: Some(5),
                },
                |_| {},
                || unreachable!(),
                || unreachable!(),
                |op, e| {
                    assert_eq!(op, 0x8a);
                    assert_eq!(e, Error::Rejected(1));
                },
            )
            .is_err()
        );
        assert_eq!(trace.into_inner(), [(0x8a, 0), (0x8b, 0)]);
    }

    #[test]
    fn cold_boot_handoff_arms_lid_close_smi_then_acpi_enable_and_disable_transition_cleanly() {
        let state = SmmState::new();
        let trace = RefCell::new(Vec::new());
        let mut poweroffs = 0usize;

        // 1. Cold-boot handoff (SCI_EN = 0): must arm legacy lid-close SMI.
        state
            .dispatch(
                &TEST_POLICY,
                4,
                SmiInput {
                    apm: Some(APM_EC_LEGACY),
                    acpi_enabled: false,
                    requested_acpi_mode: None,
                    gpi: 0,
                    sleep: None,
                },
                || Ec::discover(Ports::new(&trace)),
                |_| {},
                || poweroffs += 1,
                || unreachable!(),
                |_| unreachable!(),
            )
            .unwrap();
        assert_eq!(
            trace.borrow().as_slice(),
            &[(0x8b, 0), (0x84, 0), (0x67, 0), (0x8a, 1)]
        );
        assert!(state.smi_routed());
        assert_eq!(poweroffs, 0);

        // 2. Lid close in payload mode triggers lid_close callback.
        trace.borrow_mut().clear();
        state
            .dispatch(
                &TEST_POLICY,
                4,
                SmiInput {
                    apm: None,
                    acpi_enabled: false,
                    requested_acpi_mode: None,
                    gpi: 4,
                    sleep: None,
                },
                || {
                    let mut p = Ports::new(&trace);
                    p.left = 1; // HostEvent::LidClosed = 1
                    Ec::discover(p)
                },
                |_| {},
                || poweroffs += 1,
                || unreachable!(),
                |_| unreachable!(),
            )
            .unwrap();
        assert_eq!(poweroffs, 1);
        assert_eq!(trace.borrow().as_slice(), &[(0x84, 0), (0x84, 0)]);

        // 3. OS writes APM_CNT_ACPI_ENABLE: switches EC to SCI and disarms SMI.
        trace.borrow_mut().clear();
        state
            .dispatch(
                &TEST_POLICY,
                4,
                SmiInput {
                    apm: Some(0xa0),
                    acpi_enabled: false,
                    requested_acpi_mode: Some(true),
                    gpi: 0,
                    sleep: None,
                },
                || Ec::discover(Ports::new(&trace)),
                |_| {},
                || unreachable!(),
                || unreachable!(),
                |_| unreachable!(),
            )
            .unwrap();
        assert_eq!(
            trace.borrow().as_slice(),
            &[(0x8a, 0), (0x84, 0), (0x67, 0), (0x8b, 0x1fb)]
        );
        assert!(!state.smi_routed());

        // 4. Spurious GPI while SCI_EN=1 is ignored (OS owns EC via SCI).
        trace.borrow_mut().clear();
        state
            .dispatch(
                &TEST_POLICY,
                4,
                SmiInput {
                    apm: None,
                    acpi_enabled: true,
                    requested_acpi_mode: None,
                    gpi: 4,
                    sleep: None,
                },
                || -> Result<Ec<Ports<'_>>, Error> {
                    unreachable!("EC must not be queried when ACPI/SCI owns it")
                },
                |_| {},
                || unreachable!(),
                || unreachable!(),
                |_| unreachable!(),
            )
            .unwrap();
        assert!(trace.borrow().is_empty());

        // 5. OS writes APM_CNT_ACPI_DISABLE: re-arms legacy lid-close SMI.
        state
            .dispatch(
                &TEST_POLICY,
                4,
                SmiInput {
                    apm: Some(0xa1),
                    acpi_enabled: true,
                    requested_acpi_mode: Some(false),
                    gpi: 0,
                    sleep: None,
                },
                || Ec::discover(Ports::new(&trace)),
                |_| {},
                || unreachable!(),
                || unreachable!(),
                |_| unreachable!(),
            )
            .unwrap();
        assert_eq!(
            trace.borrow().as_slice(),
            &[(0x8b, 0), (0x84, 0), (0x67, 0), (0x8a, 1)]
        );
        assert!(state.smi_routed());

        // 6. Lid close after ACPI disable triggers lid_close callback again.
        trace.borrow_mut().clear();
        state
            .dispatch(
                &TEST_POLICY,
                4,
                SmiInput {
                    apm: None,
                    acpi_enabled: false,
                    requested_acpi_mode: None,
                    gpi: 4,
                    sleep: None,
                },
                || {
                    let mut p = Ports::new(&trace);
                    p.left = 1;
                    Ec::discover(p)
                },
                |_| {},
                || poweroffs += 1,
                || unreachable!(),
                |_| unreachable!(),
            )
            .unwrap();
        assert_eq!(poweroffs, 2);
    }

    #[test]
    fn s3_resume_handoff_preserves_sci_and_ignores_rogue_replays() {
        let state = SmmState::new();

        // S3 resume handoff arrives with SCI_EN=1: must latch policy without touching EC.
        state
            .dispatch(
                &TEST_POLICY,
                4,
                SmiInput {
                    apm: Some(APM_EC_LEGACY),
                    acpi_enabled: true,
                    requested_acpi_mode: None,
                    gpi: 0,
                    sleep: None,
                },
                || -> Result<Ec<Ports<'_>>, Error> {
                    unreachable!("S3 resume handoff must not disturb restored SCI masks")
                },
                |_| {},
                || unreachable!(),
                || unreachable!(),
                |_| unreachable!(),
            )
            .unwrap();
        assert!(!state.smi_routed());

        // Later replay of APM_EC_LEGACY (even if SCI_EN were cleared) must be ignored.
        state
            .dispatch(
                &TEST_POLICY,
                4,
                SmiInput {
                    apm: Some(APM_EC_LEGACY),
                    acpi_enabled: false,
                    requested_acpi_mode: None,
                    gpi: 4,
                    sleep: None,
                },
                || -> Result<Ec<Ports<'_>>, Error> {
                    unreachable!("rogue replay must not re-arm legacy SMI")
                },
                |_| {},
                || unreachable!(),
                || unreachable!(),
                |_| unreachable!(),
            )
            .unwrap();

        // APM_EC_OFF handoff latches disabled policy and never touches EC.
        let disabled = SmmState::new();
        for cmd in [APM_EC_OFF, APM_EC_LEGACY, APM_EC_UNIFIED] {
            disabled
                .dispatch(
                    &TEST_POLICY,
                    4,
                    SmiInput {
                        apm: Some(cmd),
                        acpi_enabled: false,
                        requested_acpi_mode: None,
                        gpi: 4,
                        sleep: None,
                    },
                    || -> Result<Ec<Ports<'_>>, Error> {
                        unreachable!("APM_EC_OFF must permanently suppress EC SMM work")
                    },
                    |_| {},
                    || unreachable!(),
                    || unreachable!(),
                    |_| unreachable!(),
                )
                .unwrap();
        }
        assert!(!disabled.smi_routed());
    }
}
