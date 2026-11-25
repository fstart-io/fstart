// SPDX-License-Identifier: MIT OR Apache-2.0
//! Qilian's sleep entry: S5 finished by the FCH SMM dispatcher with SMU
//! v14's posted SleepEntry. The common sleep path ([`crate::fch::sx`])
//! holds the shared pieces ([`Armed`]).

use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};

use crate::fch::FchHw;
use crate::fch::acpimmio::{AcpiPorts, PM_0B, PM_RESET_CONTROL1, Pm1Control, ResetControl1};
use crate::fch::smi::{self, Route, source};
use crate::fch::sx::{Armed, pm1_control};
use crate::window::{Window, update};

/// The S5 entry of the Qilian FCH (the WH SoCs), after the OS's SLP_TYP
/// write trapped to SMM ([`crate::fch::smi::route_sleep`]): the vendor's FCH
/// SMM dispatcher finishes the entry itself (Dell FchSmmDispatcher
/// `0x2a00`; FchSmmInit's power button `0x2050` the same with SLP_TYP 5;
/// decompiler): the SLP_TYP SMI status cleared (`FED80288 = 2`),
/// SlpTypeEn (`PMIO BEh |= 20h`), `PMIO 0Bh = (x & FCh) | 1`, the SLP_TYP
/// SMI off (`FED802B0 &= ~Ch`), PM1_CNT keeping SLP_TYP only (SLP_EN not
/// set), the caches written back, then SMU v14 [`SLEEP_ENTRY`] posted on
/// socket 0's SMU, and the CPU spins: the SMU powers the system off.
///
/// The vendor's S5 callbacks before it, a USB S3-entry SMU message whose
/// Qilian ID is not known and the board's wake-on-LAN, are left out.
/// Where the vendor spins for ever, this sets SLP_EN after a second
/// ([`Armed::enter`], its own fallback a full reset). Returns for other
/// types, which the vendor has no handler for either.
///
/// [`SLEEP_ENTRY`]: crate::smu::v14::msg::SLEEP_ENTRY
pub fn enter_s5_posted<H: FchHw>(hw: &H, ports: &AcpiPorts, slp_typ: u8) {
    if let Some(armed) = arm_s5_posted(hw, ports, slp_typ) {
        fstart_core::services::Timer::delay_us(hw.timer(), 1_000_000);
        armed.enter()
    }
}

/// [`enter_s5_posted`] up to the SMU's sleep entry: `None` for other types.
pub fn arm_s5_posted<'a, H: FchHw>(
    hw: &'a H,
    ports: &AcpiPorts,
    slp_typ: u8,
) -> Option<Armed<'a, H>> {
    if slp_typ != 5 {
        return None;
    }
    let (acpi, io) = (hw.acpi(), hw.io());
    smi::clear(acpi, source::SLP_TYP);
    acpi.at(PM_RESET_CONTROL1)
        .modify(ResetControl1::SLP_TYPE_EN::SET);
    update::<u8, _>(acpi, PM_0B.offset, 0x03, 0x01);
    smi::set_route(acpi, source::SLP_TYP, Route::Disabled);
    let control = pm1_control(io, ports);
    let sleep_type = control.read(Pm1Control::SLP_TYP);
    control.write(Pm1Control::SLP_TYP.val(sleep_type));
    // SAFETY: SMM owns the CPU; nothing else runs. (Host models have no
    // caches to write back.)
    #[cfg(target_os = "none")]
    unsafe {
        fstart_arch::x86::writeback_invalidate_caches()
    };
    crate::smu::v14::msg::post_sleep_entry(&hw.smn());
    Some(Armed {
        hw,
        ports: *ports,
        preparation: Ok(()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fch::acpimmio::PM;
    use crate::fch::model::fch;

    use crate::fch::IOAPIC;

    const PORTS: AcpiPorts = AcpiPorts {
        event: 0x800,
        control: 0x804,
        timer: 0x808,
        gpe: 0x820,
    };

    #[test]
    fn qilian_s5_posts_the_smu_sleep_entry() {
        let hw = fch();
        // The OS wrote SLP_TYP 5 with SLP_EN and SCI_EN (trapped).
        hw.io.put(0x804, 2, 0x1400 | 1);
        assert!(arm_s5_posted(&hw, &PORTS, 3).is_none());
        assert!(hw.io.writes_to(0x804).is_empty());
        let armed = arm_s5_posted(&hw, &PORTS, 5).unwrap();
        assert_eq!(armed.preparation, Ok(()));
        // SLP_TYP kept, SLP_EN and the rest clear.
        assert_eq!(hw.io.writes_to(0x804), vec![0x1400]);
        assert_eq!(hw.acpi.get(PM + 0xbe, 1) & 0x20, 0x20);
        assert_eq!(hw.acpi.get(PM + 0x0b, 1) & 3, 1);
        assert_eq!(hw.acpi.get(0x2b0, 4) & 0xc, 0);
        // No callbacks of the older FCHs: the IOAPIC stays.
        assert!(hw.mmio.writes_to(IOAPIC + 0x10).is_empty());
        // SMU v14 message 0Bh, its response cleared, arguments 0.
        let writes = crate::fch::model::smn_writes(&hw.pci, 0);
        assert_eq!(writes.last(), Some(&(0x03b1_0948, 0x0b)));
        assert!(writes.contains(&(0x03b1_094c, 0)));
        assert!(writes.contains(&(0x03b1_0950, 0)));
    }
}
