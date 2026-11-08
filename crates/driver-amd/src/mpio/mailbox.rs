// SPDX-License-Identifier: MIT OR Apache-2.0
//! Siena AmdNbioPei `ffcfa331`: bit31-ready mailbox, six arguments,
//! request in bits19:8, then a separate doorbell. Unlike the vendor code,
//! all loops are bounded,
//! all-ones is unavailable, and a rejected response is an error.

use crate::smn::{SmnAccess, SmnReg};
use core::num::NonZeroU32;
use embedded_hal::delay::DelayNs;
use tock_registers::{
    LocalRegisterCopy,
    interfaces::{Readable, Writeable},
    register_bitfields,
};

register_bitfields![u32,
    pub Control [
        STATUS OFFSET(0) NUMBITS(8) [],
        REQUEST OFFSET(8) NUMBITS(12) [],
        READY OFFSET(31) NUMBITS(1) []
    ]
];

pub const CONTROL: SmnReg<Control::Register> = SmnReg::new(0x0c91_09c8);
pub const ARGUMENTS: SmnReg = SmnReg::new(0x0c91_09cc);
/// Turin uses 0x0c910954 instead. Do not change this for Siena.
pub const DOORBELL: SmnReg = SmnReg::new(0x0c91_0554);
pub const ARGUMENT_COUNT: usize = 6;

pub mod msg {
    pub const STATUS: u16 = 1;
    pub const GLOBAL_CONFIG: u16 = 2;
    pub const GET_ASK: u16 = 3;
    pub const TRANSFER_ASK: u16 = 7;
    pub const EXT_ATTRIBUTES: u16 = 8;
    pub const SETUP_LINK: u16 = 0x304;
    pub const UPDATE_LINK: u16 = 0x322;
}

/// SETUP_LINK's argument0 masks, not additional request IDs.
pub mod phase {
    pub const MAP: u32 = 1;
    pub const CONFIGURE: u32 = 6;
    pub const PERST: u32 = 8;
    pub const TRAIN: u32 = 0x30;
}

/// A finite number of SMN reads for each ready/completion wait. Time spent
/// in SMN accesses is additional to the requested inter-poll delays.
#[derive(Clone, Copy)]
pub struct Poll {
    pub reads: NonZeroU32,
    pub interval_us: NonZeroU32,
}
impl Default for Poll {
    fn default() -> Self {
        Self {
            reads: NonZeroU32::new(100_000).unwrap(),
            interval_us: NonZeroU32::new(100).unwrap(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    InvalidRequest(u16),
    ReadyTimeout,
    CompletionTimeout,
    IdleTimeout,
    Unavailable,
    Rejected(u8),
    /// A previous transport failure left firmware ownership unresolved.
    Poisoned,
    /// Empty/oversized alias view list or an invalid reference index.
    InvalidView,
}

/// Discover aliases of a reference mailbox among at most eight host-bridge
/// views. Two different cookies avoid mistaking an unrelated constant value
/// for an alias. Restore the exact argument word and verify every view.
/// No control/doorbell write or firmware command is issued.
///
/// # Safety
/// The caller exclusively owns ALL views and has established protocol idle
/// on them. Argument5 is host-writable, with no outstanding request or DMA.
/// On any error abort initialization; never use a failed restore as permission
/// to create a fresh session or submit another operation.
pub unsafe fn alias_mask<A: SmnAccess>(views: &[A], reference: usize) -> Result<u8, Error> {
    if views.is_empty() || views.len() > 8 || reference >= views.len() {
        return Err(Error::InvalidView);
    }
    let scratch = ARGUMENTS.offset(4 * (ARGUMENT_COUNT as u32 - 1));
    let mut previous = [0u32; 8];
    for (i, view) in views.iter().enumerate() {
        let control = view.reg(CONTROL).get();
        if control == u32::MAX {
            return Err(Error::Unavailable);
        }
        if !Control::READY.is_set(control) {
            return Err(Error::ReadyTimeout);
        }
        previous[i] = view.reg(scratch).get();
    }
    let mut aliases = ((1u16 << views.len()) - 1) as u8;
    for cookie in [!previous[reference], previous[reference] ^ 0xa5a5_5a5a] {
        views[reference].reg(scratch).set(cookie);
        for (i, view) in views.iter().enumerate() {
            if view.reg(scratch).get() != cookie {
                aliases &= !(1 << i);
            }
        }
    }
    views[reference].reg(scratch).set(previous[reference]);
    for (i, view) in views.iter().enumerate() {
        if view.reg(scratch).get() != previous[i] {
            return Err(Error::Poisoned);
        }
    }
    if aliases & (1 << reference) == 0 {
        return Err(Error::Unavailable);
    }
    Ok(aliases)
}

/// Exclusive use of one selected SMN space's MPIO mailbox. A transport
/// failure poisons the session: the firmware might still own DMA buffers.
/// Do not create a replacement session to bypass a timeout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    /// S8040/NFX's retained interface, including the posted-request bits.
    Retained,
    /// Onyx installed MpioServiceRequestCommon at20042e10 masks the
    /// shifted request withFFFFh; openSIL MpioLib.c also keeps only byte0.
    LowByte,
}
pub struct Mailbox {
    poll: Poll,
    encoding: Encoding,
    poisoned: bool,
}
impl Mailbox {
    /// # Safety
    /// `smn` supplied to subsequent calls always selects the same Siena
    /// instance, exclusively owned for this session. It is idle or a prior
    /// firmware operation has completed. Recovery after a timeout requires
    /// re-establishing firmware/DMA ownership, not merely reconstructing this.
    pub const unsafe fn new(poll: Poll) -> Self {
        unsafe { Self::new_with_encoding(poll, Encoding::Retained) }
    }
    /// Same ownership requirements as [`Self::new`]. Select the verified
    /// command encoding before the first request, never after a failure.
    /// # Safety
    /// Caller verifies the selected firmware interface and owns the controller.
    pub const unsafe fn new_with_encoding(poll: Poll, encoding: Encoding) -> Self {
        Self {
            poll,
            encoding,
            poisoned: false,
        }
    }

    fn ready(&self, smn: &impl SmnAccess, delay: &mut impl DelayNs) -> Result<u32, Error> {
        for i in 0..self.poll.reads.get() {
            let value = smn.reg(CONTROL).get();
            if value == u32::MAX {
                return Err(Error::Unavailable);
            }
            if Control::READY.is_set(value) {
                return Ok(value);
            }
            if i + 1 < self.poll.reads.get() {
                delay.delay_us(self.poll.interval_us.get());
            }
        }
        Err(Error::ReadyTimeout)
    }

    /// Send and collect all six reply words. Invalid IDs (including the
    /// donor's silent no-op 0xff) are rejected before any SMN access.
    ///
    /// # Safety
    /// Arguments obey the retained firmware's command ABI. Pointed buffers
    /// are physically addressable, coherent, reserved and live until firmware
    /// releases them. A timeout does NOT release those buffers. This raw
    /// transport neither allocates DMA memory nor establishes its lifetime.
    pub unsafe fn request(
        &mut self,
        smn: &impl SmnAccess,
        request: u16,
        arguments: [u32; ARGUMENT_COUNT],
        delay: &mut impl DelayNs,
    ) -> Result<[u32; ARGUMENT_COUNT], Error> {
        if self.poisoned {
            return Err(Error::Poisoned);
        }
        if request == 0 || request == 0xff || request > 0xfff {
            return Err(Error::InvalidRequest(request));
        }
        let wire = match self.encoding {
            Encoding::Retained => request,
            Encoding::LowByte => request & 0xff,
        };
        if wire == 0 || wire == 0xff {
            return Err(Error::InvalidRequest(request));
        }
        let result = (|| {
            self.ready(smn, delay)?;
            for (i, value) in arguments.into_iter().enumerate() {
                smn.reg(ARGUMENTS.offset(4 * i as u32)).set(value);
            }
            smn.reg(CONTROL).write(Control::REQUEST.val(wire.into()));
            smn.reg(DOORBELL).set(u32::MAX);
            let value = self.ready(smn, delay).map_err(|e| match e {
                Error::ReadyTimeout => Error::CompletionTimeout,
                e => e,
            })?;
            // Collect arguments even when the firmware rejects the request.
            let reply = core::array::from_fn(|i| smn.reg(ARGUMENTS.offset(4 * i as u32)).get());
            let status =
                LocalRegisterCopy::<u32, Control::Register>::new(value).read(Control::STATUS) as u8;
            if status != 1 {
                return Err(Error::Rejected(status));
            }
            Ok(reply)
        })();
        if matches!(
            result,
            Err(Error::ReadyTimeout | Error::CompletionTimeout | Error::Unavailable)
        ) {
            self.poisoned = true;
        }
        result
    }

    /// Pointer-free STATUS query, retaining all six reply words for
    /// diagnostics. It does not submit configuration or start training.
    pub fn status(
        &mut self,
        smn: &impl SmnAccess,
        delay: &mut impl DelayNs,
    ) -> Result<[u32; ARGUMENT_COUNT], Error> {
        // SAFETY: STATUS's zero arguments contain no addresses.
        unsafe { self.request(smn, msg::STATUS, [0; ARGUMENT_COUNT], delay) }
    }

    /// Status command has no pointers. Bound the protocol-level idle loop
    /// too: a responsive mailbox can indefinitely report a busy firmware.
    pub fn wait_idle(
        &mut self,
        smn: &impl SmnAccess,
        delay: &mut impl DelayNs,
        attempts: NonZeroU32,
    ) -> Result<(), Error> {
        for i in 0..attempts.get() {
            let reply = self.status(smn, delay)?;
            if reply[0] == 0 {
                return Ok(());
            }
            if i + 1 < attempts.get() {
                delay.delay_us(self.poll.interval_us.get());
            }
        }
        self.poisoned = true;
        Err(Error::IdleTimeout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::smn::{SMN_DATA, Smn};
    use crate::{ccx::tsc::testing::Elapsed, smn::model::Model};
    use fstart_pci::{ConfigRegionAccess, PciAddress};

    #[derive(Clone)]
    struct Firmware {
        model: Model,
        status: u32,
        busy: bool,
    }
    impl ConfigRegionAccess for Firmware {
        unsafe fn read(&self, a: PciAddress, off: u16) -> u32 {
            unsafe { self.model.read(a, off) }
        }
        unsafe fn write(&self, a: PciAddress, off: u16, value: u32) {
            unsafe { self.model.write(a, off, value) };
            let mut state = self.model.0.borrow_mut();
            if off == SMN_DATA && state.index[&a.bus()] == DOORBELL.address() {
                state.smn.insert((a.bus(), CONTROL.address()), self.status);
                for i in 0..6 {
                    state.smn.insert(
                        (a.bus(), ARGUMENTS.offset(i * 4).address()),
                        if self.busy { 1 } else { i + 10 },
                    );
                }
            }
        }
    }
    fn setup(status: u32) -> (Firmware, Smn<Firmware>, Mailbox) {
        let fw = Firmware {
            model: Model::default(),
            status,
            busy: false,
        };
        fw.model
            .0
            .borrow_mut()
            .smn
            .insert((0x80, CONTROL.address()), 1 << 31);
        let smn = unsafe { Smn::on_bus(fw.clone(), 0x80) };
        let mailbox = unsafe {
            Mailbox::new(Poll {
                reads: NonZeroU32::new(3).unwrap(),
                interval_us: NonZeroU32::new(2).unwrap(),
            })
        };
        (fw, smn, mailbox)
    }

    #[test]
    fn aliases_are_not_roots_and_two_cookies_exclude_a_coincidental_constant() {
        use crate::smn::testing::SmnSpace;
        fn only_scratch(_: &SmnSpace, address: u32, _: u32) {
            assert_eq!(address, ARGUMENTS.offset(20).address());
        }
        let mut a = SmnSpace::default();
        a.on_write = Some(only_scratch);
        let b = SmnSpace::default();
        for space in [&a, &b] {
            space.set(CONTROL, 1 << 31);
        }
        a.set(ARGUMENTS.offset(20), 0x12345678);
        b.set(ARGUMENTS.offset(20), !0x12345678);
        let views = [a.smn(), b.smn(), a.smn()];
        assert_eq!(unsafe { alias_mask(&views, 0) }, Ok(0b101));
        assert_eq!(a.get(ARGUMENTS.offset(20)), 0x12345678);
        assert_eq!(b.get(ARGUMENTS.offset(20)), !0x12345678);
        let eight = core::array::from_fn::<_, 8, _>(|_| a.smn());
        assert_eq!(unsafe { alias_mask(&eight, 7) }, Ok(0xff));
        assert_eq!(
            unsafe { alias_mask(&views[..0], 0) },
            Err(Error::InvalidView)
        );
        assert_eq!(unsafe { alias_mask(&views, 3) }, Err(Error::InvalidView));
    }

    #[test]
    fn alias_preflight_never_writes_a_busy_view_and_restore_failure_is_fatal() {
        use crate::smn::testing::SmnSpace;
        let mut a = SmnSpace::default();
        let b = SmnSpace::default();
        a.set(CONTROL, 1 << 31);
        a.set(ARGUMENTS.offset(20), 42);
        assert_eq!(
            unsafe { alias_mask(&[a.smn(), b.smn()], 0) },
            Err(Error::ReadyTimeout)
        );
        assert_eq!(a.get(ARGUMENTS.offset(20)), 42);
        a.on_write = Some(|space, address, value| {
            if value == 42 {
                space.set(address, 43);
            }
        });
        assert_eq!(unsafe { alias_mask(&[a.smn()], 0) }, Err(Error::Poisoned));
    }

    #[test]
    fn exact_order_and_six_arguments_on_selected_root_bus() {
        let (fw, smn, mut mailbox) = setup(0x8000_0001);
        let reply = unsafe {
            mailbox.request(
                &smn,
                msg::SETUP_LINK,
                [1, 2, 3, 4, 5, 6],
                &mut Elapsed::default(),
            )
        }
        .unwrap();
        assert_eq!(reply, [10, 11, 12, 13, 14, 15]);
        let log = &fw.model.0.borrow().log;
        assert_eq!(log.len(), 8);
        for i in 0..6 {
            assert_eq!(
                log[i],
                (0x80, ARGUMENTS.offset(4 * i as u32).address(), i as u32 + 1)
            );
        }
        assert_eq!(log[6], (0x80, CONTROL.address(), 0x30400));
        assert_eq!(log[7], (0x80, DOORBELL.address(), u32::MAX));
    }

    #[test]
    fn genoa_posted_flags_are_host_only_not_control_bits() {
        let (fw, smn, original) = setup(0x8000_0001);
        let mut mailbox = unsafe { Mailbox::new_with_encoding(original.poll, Encoding::LowByte) };
        let mut delay = Elapsed::default();
        unsafe { mailbox.request(&smn, msg::SETUP_LINK, [6, 0, 0, 0, 0, 0], &mut delay) }.unwrap();
        unsafe { mailbox.request(&smn, msg::UPDATE_LINK, [0; 6], &mut delay) }.unwrap();
        assert_eq!(fw.model.0.borrow().log[6], (0x80, CONTROL.address(), 0x400));
        assert_eq!(
            fw.model.0.borrow().log[14],
            (0x80, CONTROL.address(), 0x2200)
        );
        let count = fw.model.0.borrow().log.len();
        for id in [0x300, 0x3ff] {
            assert_eq!(
                unsafe { mailbox.request(&smn, id, [0; 6], &mut delay) },
                Err(Error::InvalidRequest(id))
            );
        }
        assert_eq!(fw.model.0.borrow().log.len(), count);
    }

    #[test]
    fn bounded_failures_poison_and_do_not_submit_again() {
        for (value, error) in [
            (0, Error::CompletionTimeout),
            (u32::MAX, Error::Unavailable),
        ] {
            let (fw, smn, mut mailbox) = setup(value);
            let mut delay = Elapsed::default();
            assert_eq!(
                unsafe { mailbox.request(&smn, 7, [0; 6], &mut delay) },
                Err(error)
            );
            let count = fw.model.0.borrow().log.len();
            assert_eq!(
                unsafe { mailbox.request(&smn, 7, [0; 6], &mut delay) },
                Err(Error::Poisoned)
            );
            assert_eq!(fw.model.0.borrow().log.len(), count);
            assert!(delay.0 <= 4);
        }
        let (fw, smn, mut mailbox) = setup(0x8000_0001);
        fw.model
            .0
            .borrow_mut()
            .smn
            .insert((0x80, CONTROL.address()), 0);
        assert_eq!(
            unsafe { mailbox.request(&smn, 7, [0; 6], &mut Elapsed::default()) },
            Err(Error::ReadyTimeout)
        );
        assert!(fw.model.0.borrow().log.is_empty());
    }

    #[test]
    fn invalid_ids_do_not_touch_hardware_and_rejections_are_not_success() {
        let (fw, smn, mut mailbox) = setup(0x8000_00fe);
        for id in [0, 0xff, 0x1000] {
            assert_eq!(
                unsafe { mailbox.request(&smn, id, [0; 6], &mut Elapsed::default()) },
                Err(Error::InvalidRequest(id))
            );
        }
        assert!(fw.model.0.borrow().log.is_empty());
        assert_eq!(
            unsafe { mailbox.request(&smn, 7, [0; 6], &mut Elapsed::default()) },
            Err(Error::Rejected(0xfe))
        );
    }

    #[test]
    fn responsive_but_never_idle_is_bounded() {
        let (mut fw, _, mut mailbox) = setup(0x8000_0001);
        fw.busy = true;
        let smn = unsafe { Smn::on_bus(fw.clone(), 0x80) };
        assert_eq!(
            mailbox.wait_idle(&smn, &mut Elapsed::default(), NonZeroU32::new(3).unwrap()),
            Err(Error::IdleTimeout)
        );
        assert_eq!(fw.model.0.borrow().log.len(), 3 * 8);
    }
}
