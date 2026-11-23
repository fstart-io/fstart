// SPDX-License-Identifier: MIT OR Apache-2.0
//! SMU v14 (WH MP1): 4-argument mailbox (`0x03B1_0948` message, `0x03B1_094C`
//! response, `0x03B1_0950..=0x03B1_095C` arguments 0..=3, IDs `< 0x5B`, fatal
//! status `0xFFFF_FFFC` skipping argument readback). Reuses the common
//! [`super::Mailbox`] / [`super::Smu`] transport.

use super::Mailbox;
use crate::smn::SmnReg;

/// Number of argument registers in the SMU v14 mailbox.
pub const ARGUMENT_COUNT: usize = 4;
/// Exclusive upper bound on valid SMU v14 message IDs (`< 0x5B`).
pub const ID_LIMIT: u32 = 0x5b;

pub const MAILBOX: Mailbox = Mailbox {
    message: SmnReg::new(0x03b1_0948),
    response: SmnReg::new(0x03b1_094c),
    arguments: SmnReg::new(0x03b1_0950),
    wait_before: true,
    argument_count: ARGUMENT_COUNT,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::smn::testing::{SmnModel, SmnTest};
    use crate::smu::{Error, SmuMessages, model::SmuModel, response};

    #[derive(Clone, Copy)]
    struct NoDelay;
    impl embedded_hal::delay::DelayNs for NoDelay {
        fn delay_ns(&mut self, _: u32) {}
    }

    #[test]
    fn message_bound_and_common_transport() {
        let m = SmuModel::new(MAILBOX, response::OK);
        let smu = m.smu(ID_LIMIT);
        smu.send(0x5a, &mut [0; 6]).unwrap();
        assert_eq!(
            smu.send(ID_LIMIT, &mut [0; 6]),
            Err(Error::Rejected(response::UNKNOWN_CMD))
        );
        assert_eq!(m.ids(), [0x5a]);
    }

    #[test]
    fn four_argument_window_and_fatal_skips_readback() {
        struct FakeV14 {
            resp: u32,
            next_resp: u32,
            args: [u32; 4],
            arg_writes: u32,
            arg_reads: u32,
        }
        impl SmnModel for FakeV14 {
            fn read(&mut self, addr: u32) -> u32 {
                if addr == MAILBOX.response.address() {
                    self.resp
                } else if (MAILBOX.arguments.address()..MAILBOX.arguments.address() + 16)
                    .contains(&addr)
                {
                    self.arg_reads += 1;
                    self.args[((addr - MAILBOX.arguments.address()) / 4) as usize]
                } else {
                    panic!("unexpected read beyond 4-arg window: {addr:#x}");
                }
            }
            fn write(&mut self, addr: u32, value: u32) {
                if addr == MAILBOX.response.address() {
                    self.resp = value;
                } else if addr == MAILBOX.message.address() {
                    self.resp = self.next_resp;
                    if self.next_resp == response::OK {
                        self.args[0] = self.args[0].wrapping_add(1);
                    }
                } else if (MAILBOX.arguments.address()..MAILBOX.arguments.address() + 16)
                    .contains(&addr)
                {
                    self.arg_writes += 1;
                    self.args[((addr - MAILBOX.arguments.address()) / 4) as usize] = value;
                } else {
                    panic!("unexpected write beyond 4-arg window: {addr:#x}");
                }
            }
        }

        let ok = SmnTest::new(FakeV14 {
            resp: response::OK,
            next_resp: response::OK,
            args: [0; 4],
            arg_writes: 0,
            arg_reads: 0,
        });
        let reply = MAILBOX
            .request(
                &ok.smn(),
                0x01,
                [0xaa55_aa55, 1, 2, 3, 99, 99],
                &mut NoDelay,
            )
            .unwrap();
        assert_eq!(reply, [0xaa55_aa56, 1, 2, 3, 0, 0]);
        assert_eq!(ok.model().arg_writes, 4);
        assert_eq!(ok.model().arg_reads, 4);

        let fatal = SmnTest::new(FakeV14 {
            resp: response::OK,
            next_resp: response::FATAL,
            args: [0; 4],
            arg_writes: 0,
            arg_reads: 0,
        });
        assert_eq!(
            MAILBOX.request(&fatal.smn(), 0x02, [0; 6], &mut NoDelay),
            Err(Error::Rejected(response::FATAL))
        );
        assert_eq!(fatal.model().arg_reads, 0);
    }
}
