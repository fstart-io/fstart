// SPDX-License-Identifier: MIT OR Apache-2.0
//! HSMP (Host System Management Port) for the OS: one `AMDI0097` device per
//! socket and the `HSMP` table, as AMD's `SMU-HSMP` SSDT and HsmpWhDxe
//! (Dell PR9XJ `0x1c60`, GIGABYTE same strings; DEVICES.md §2.11 [D])
//! publish them on Venice.
//!
//! The vendor's SSDT holds placeholders HsmpWhDxe patches (`0x1060`): in
//! `HSn` (`n` the socket) `$HE<n>` becomes 1 (`_STA` 0xF), `$HM<n>` the
//! socket's SMU BAR (`Memory32Fixed`, 1 MiB), `$MI<n>`, `$MR<n>`, `$MO<n>`
//! the mailbox offsets in `_DSD` (`MsgIdOffset`, `MsgRspOffset`,
//! `MsgArgOffset`, UUID `b74d619d-5707-48bd-a69f-4ea2871fc2f6`), with
//! `_UID "IDnn"`. Here the devices are built with the values instead. The
//! `HSMP` table (revision 1, OEM `AMDINC`/`AMDCRB` in the vendor's) has a
//! 0x20-byte entry per socket: the instance (bits 3:0), the BAR's low
//! half and six offsets ([`fstart_driver_amd::smu::v14::msg::HsmpOffsets`]).
//! Linux's `amd_hsmp` ACPI driver reads the devices.
//!
//! Only SP7 reaches this module: its functions are `#[inline]`, so their
//! strings stay out of other SoCs' images (docs/amd-structure.md).

extern crate alloc;

use alloc::vec::Vec;
use fstart_acpi::aml_linker::device_vec;
use fstart_acpi::{AmlError, acpi_dsl, sdt::Sdt};

/// The SMU window each device's `_CRS` names.
pub const SMU_WINDOW: u32 = 1 << 20;

/// One socket's HSMP mailbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Instance {
    pub socket: u8,
    /// The SMU BAR (below 4 GiB, as the vendor's `Memory32Fixed`).
    pub bar: u32,
    /// The mailbox offsets in the SMU window (message ID, response,
    /// argument, then the table's other three).
    pub offsets: [u32; 6],
}

/// `HSn` for `\_SB`: `_HID "AMDI0097"`, `_UID "IDnn"`, `_STA` 0xF, the SMU
/// window and the mailbox offsets.
#[inline]
pub fn device(i: &Instance) -> Result<Vec<u8>, AmlError> {
    let socket = i.socket;
    if socket > 9 {
        return Err(AmlError::InvalidOperand);
    }
    let (bar, id, rsp, arg) = (
        u64::from(i.bar),
        u64::from(i.offsets[0]),
        u64::from(i.offsets[1]),
        u64::from(i.offsets[2]),
    );
    // Name (_UID, "ID0n"): the macro takes no run-time strings.
    let mut body = alloc::vec![
        0x08,
        b'_',
        b'U',
        b'I',
        b'D',
        0x0d,
        b'I',
        b'D',
        b'0',
        b'0' + socket,
        0,
    ];
    acpi_dsl! {
        Name("_HID", "AMDI0097");
        Method("_STA", 0, NotSerialized) {
            Return(0x0Fu8);
        }
        Name("_CRS", ResourceTemplate {
            Memory32Fixed(ReadWrite, #{dword bar}, #{const dword SMU_WINDOW});
        });
        Name("_DSD", Package(
            ToUUID("b74d619d-5707-48bd-a69f-4ea2871fc2f6"),
            Package(
                Package("MsgIdOffset", #{dword id}),
                Package("MsgRspOffset", #{dword rsp}),
                Package("MsgArgOffset", #{dword arg}),
            )
        ));
    }
    .emit(&mut body)?;
    let name = alloc::format!("HS0{}", socket);
    device_vec(&name, &body)
}

/// The `HSMP` table of `instances`.
#[inline]
pub fn table(instances: &[Instance]) -> Vec<u8> {
    let mut sdt = Sdt::new(
        *b"HSMP",
        36,
        1,
        fstart_acpi::OEM_ID,
        fstart_acpi::OEM_TABLE_ID,
        fstart_acpi::OEM_REVISION,
    );
    for i in instances {
        sdt.append_slice(&u32::from(i.socket & 0xf).to_le_bytes());
        sdt.append_slice(&i.bar.to_le_bytes());
        for offset in i.offsets {
            sdt.append_slice(&offset.to_le_bytes());
        }
    }
    sdt.update_checksum();
    fstart_acpi::serialize(&sdt)
}

#[cfg(test)]
mod tests {
    use super::*;

    const S0: Instance = Instance {
        socket: 1,
        bar: 0xaff0_0000,
        offsets: [0x1_0934, 0x1_0980, 0x1_09e0, 0x1_0000, 0x1_0000, 0x1_0000],
    };

    #[test]
    fn device_and_table() {
        let aml = device(&S0).unwrap();
        for s in [&b"HS01"[..], b"AMDI0097", b"ID01", b"MsgRspOffset"] {
            assert!(aml.windows(s.len()).any(|w| w == s), "{s:?}");
        }
        // Memory32Fixed(ReadWrite, 0xAFF00000, 1 MiB).
        let fixed = [0x86, 9, 0, 1, 0, 0, 0xf0, 0xaf, 0, 0, 0x10, 0];
        assert!(aml.windows(12).any(|w| w == fixed));
        let t = table(&[S0]);
        assert_eq!(&t[..4], b"HSMP");
        assert_eq!(t.len(), 36 + 32);
        assert_eq!(u32::from_le_bytes(t[4..8].try_into().unwrap()), 68);
        assert_eq!(t[36], 1);
        assert_eq!(&t[40..44], &0xaff0_0000u32.to_le_bytes());
        assert_eq!(t.iter().fold(0u8, |a, b| a.wrapping_add(*b)), 0);
    }
}
