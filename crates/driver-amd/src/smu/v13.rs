// SPDX-License-Identifier: MIT OR Apache-2.0
//! Rs SMU mailbox. S8040 V2.00 AmdNbioPei `ffcfefc8` and Genoa's public
//! SMU/Common/SmuCommon.h agree on these registers. These are not Turin's
//! addresses. Individual message IDs/range policy still need SoC validation.
//!
//! Use the common [`super::Smu`] transport with [`MAILBOX`] and
//! [`ID_LIMIT`]; no duplicate SMN or six-argument sender is needed.

use super::Mailbox;
use crate::smn::SmnReg;

pub const MAILBOX: Mailbox = Mailbox {
    message: SmnReg::new(0x03b1_0530),
    response: SmnReg::new(0x03b1_057c),
    arguments: SmnReg::new(0x03b1_09c4),
    wait_before: false,
};

/// Reject, rather than silently reporting success, for IDs >= 0x51.
pub const ID_LIMIT: u32 = 0x51;
/// Installed Genoa sender2005ea27 rejects IDs above47h (exclusive bound).
pub const GENOA_ID_LIMIT: u32 = 0x48;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::smu::{Error, SmuMessages, model::SmuModel, response};

    #[test]
    fn siena_range_and_common_transport() {
        let m = SmuModel::new(MAILBOX, response::OK);
        let smu = m.smu(ID_LIMIT);
        smu.send(0x50, &mut [0; 6]).unwrap();
        assert_eq!(
            smu.send(0x51, &mut [0; 6]),
            Err(Error::Rejected(response::UNKNOWN_CMD))
        );
        assert_eq!(m.ids(), [0x50]);
    }
}
