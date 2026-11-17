// SPDX-License-Identifier: MIT OR Apache-2.0
//! SMU v13 (AmdNbioSmuV13: Genoa, Siena): its mailbox registers and the
//! message IDs its firmware takes. The S8040's V2.00 AmdNbioPei
//! (`ffcfefc8`) and Genoa's public `SMU/Common/SmuCommon.h` agree on the
//! registers. The common transport is [`super::Smu`] with [`MAILBOX`].

use super::Mailbox;
use crate::smn::SmnReg;

pub const MAILBOX: Mailbox = Mailbox {
    message: SmnReg::new(0x03b1_0530),
    response: SmnReg::new(0x03b1_057c),
    arguments: SmnReg::new(0x03b1_09c4),
    wait_before: false,
};

/// GetSmuVersion: the firmware revision in argument 0.
pub const GET_VERSION: u32 = 0x02;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::smu::{Error, SmuMessages, model::SmuModel, response};

    #[test]
    fn message_bound_and_common_transport() {
        let m = SmuModel::new(MAILBOX, response::OK);
        let smu = m.smu(0x51);
        smu.send(0x50, &mut [0; 6]).unwrap();
        assert_eq!(
            smu.send(0x51, &mut [0; 6]),
            Err(Error::Rejected(response::UNKNOWN_CMD))
        );
        assert_eq!(m.ids(), [0x50]);
    }
}
