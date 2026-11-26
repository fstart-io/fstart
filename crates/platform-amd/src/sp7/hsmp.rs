// SPDX-License-Identifier: MIT OR Apache-2.0
//! HSMP on SP7 (`HsmpWhDxe`, DEVICES.md §2.11): each socket's SMU window
//! (IOHC 0's SMU BAR, `init_fch` places one where none was decoded) and its
//! mailbox offsets (SMU message `0x33`), for `acpi::hsmp`'s devices and
//! table.

extern crate alloc;

use alloc::vec::Vec;

use fstart_driver_amd::smu::v14::msg;

use super::init::State;
use super::nbio::{part, rb_root, root_bridge};
use super::{Sp7, Variant};
use crate::acpi::hsmp::Instance;
use crate::flow::Ctx;

/// Every socket's HSMP mailbox that has an SMU window below 4 GiB and
/// answers message `0x33`; none without [`super::Devices::hsmp`].
pub(super) fn instances<V: Variant>(ctx: &Ctx<'_, Sp7<V>>, state: &State) -> Vec<Instance> {
    let mut out = Vec::new();
    if !ctx.config.devices.hsmp {
        return out;
    }
    let part = part();
    for socket in 0..state.sockets() {
        // IOHC 0 of the socket: die 0, root bridge 0.
        let Some(nb) = state
            .roots()
            .iter()
            .find(|rb| (rb.socket, rb.index) == (socket, 0))
            .and_then(|rb| root_bridge(part, rb.index, rb_root(rb)))
        else {
            continue;
        };
        let Some(bar) = nb.smu_bar().and_then(|b| u32::try_from(b & !3).ok()) else {
            fstart_log::warn!("{}: S{} no SMU BAR below 4 GiB: no HSMP", V::NAME, socket);
            continue;
        };
        match msg::hsmp_offsets(&state.smu_on(socket, 0, ctx.clock)) {
            Ok(offsets) => out.push(Instance {
                socket,
                bar,
                offsets: offsets.offsets,
            }),
            Err(_) => fstart_log::warn!("{}: S{} SMU HSMP offsets refused", V::NAME, socket),
        }
    }
    out
}
