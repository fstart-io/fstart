// SPDX-License-Identifier: MIT OR Apache-2.0
//! PCH-owned native DMI half, sa_pre_mem_native.c:459–490,635–686.
//! Source SHA256: 27e7566c9724ee95db1b69db040a64b5211a8460eb789b1758f7ebd2cf06a372.
//! All writes use PCR's existing same-dword posted completion transport.
use super::pcr::{Pcr, Port};
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};
use tock_registers::register_bitfields;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    H,
    Lp,
}
pub(crate) mod post_memory;
register_bitfields![u32,
    pub LinkCap [
        SPEED OFFSET(0) NUMBITS(4) [],
        ASPM OFFSET(10) NUMBITS(2) [],
        L0_EXIT OFFSET(12) NUMBITS(3) [],
        L1_EXIT OFFSET(15) NUMBITS(3) []
    ],
    pub Target [ SPEED OFFSET(0) NUMBITS(4) [] ],
    pub Hints [
        CLEAR_LOW OFFSET(4) NUMBITS(4) [],
        SET_LOW OFFSET(8) NUMBITS(3) [],
        HIGH OFFSET(24) NUMBITS(4) []
    ],
    pub EqConfig1 [ ENABLE OFFSET(16) NUMBITS(1) [] ],
    pub Sustain [ ENABLE OFFSET(9) NUMBITS(1) [] ],
    pub VcStatusPair [ PENDING OFFSET(17) NUMBITS(1) [] ],
    pub VcControl [
        TC0 OFFSET(0) NUMBITS(1) [],
        MAP OFFSET(1) NUMBITS(7) [],
        ID OFFSET(24) NUMBITS(4) [],
        ENABLE OFFSET(31) NUMBITS(1) []
    ]
];

pub(crate) fn max_speed(pcr: &Pcr) -> u8 {
    pcr.register::<LinkCap::Register>(Port::Dmi, 0x21a4)
        .unwrap()
        .read(LinkCap::SPEED) as u8
}

pub(crate) fn before_speed_change(pcr: &Pcr, speed: u8) {
    if speed > 2 {
        for offset in [0x21bc, 0x21c0] {
            let r = pcr.register::<Hints::Register>(Port::Dmi, offset).unwrap();
            // Deliberately retain native's mask 0xf0 and set shift 8 (not 4).
            // Bits 10:8 are ORed, not replaced. No undocumented repair here.
            r.set(
                (r.get() & !(Hints::CLEAR_LOW.val(15).value | Hints::HIGH.val(15).value))
                    | Hints::SET_LOW.val(7).value
                    | Hints::HIGH.val(7).value,
            );
        }
        pcr.register::<EqConfig1::Register>(Port::Dmi, 0x2450)
            .unwrap()
            .modify(EqConfig1::ENABLE::SET);
    }
    pcr.register::<Sustain::Register>(Port::Dmi, 0x2338)
        .unwrap()
        .modify(Sustain::ENABLE::SET);
    let cap = max_speed(pcr);
    // Exact native conditions, not an invented min(target, cap) fallback.
    if (speed >= 3 && cap == 3) || (speed == 2 && cap >= 2) {
        pcr.register::<Target::Register>(Port::Dmi, 0x21b0)
            .unwrap()
            .modify(Target::SPEED.val(u32::from(speed)));
    }
}

pub(crate) fn link_status(pcr: &Pcr) -> u16 {
    // Pcr's transport is dword-only. This reads the aligned LCTL/LSTS pair;
    // neither half has read side effects. No writeback of status/control.
    (pcr.register::<()>(Port::Dmi, 0x21a8).unwrap().get() >> 16) as u16
}

pub(crate) fn configure_vcs(pcr: &Pcr, vc1: bool) {
    let tvm0 = if vc1 { 1 } else { 3 };
    for (offset, id, tc) in [(0x2014, 0, tvm0), (0x2020, 1, 2), (0x2040, 7, 128)] {
        if offset == 0x2020 && !vc1 {
            continue;
        }
        let r = pcr
            .register::<VcControl::Register>(Port::Dmi, offset)
            .unwrap();
        let fields = VcControl::MAP.val(tc >> 1) + VcControl::ID.val(id) + VcControl::ENABLE::SET;
        // Native clears bits 7:1 but ORs bit0, preserving any old TC0 bit.
        if offset == 0x2014 {
            r.modify(fields + VcControl::TC0::SET);
        } else {
            r.modify(fields);
        }
    }
}

pub(crate) fn vcs_pending(pcr: &Pcr, vc1: bool) -> bool {
    [0x2018, 0x2024, 0x2044]
        .into_iter()
        .enumerate()
        .any(|(i, offset)| {
            (i != 1 || vc1)
                && pcr
                    .register::<VcStatusPair::Register>(Port::Dmi, offset)
                    .unwrap()
                    .is_set(VcStatusPair::PENDING)
        })
}
