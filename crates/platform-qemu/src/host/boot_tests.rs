//! What each QEMU machine is booted with in the boot tests, and what it must
//! print: the lines its stages log (`crate::messages`, the arch's), and what
//! the guests of the boot assets print.

use crate::facts::{VirtBoardFacts, VirtMachine};
use fstart_arch::messages::{
    MP_COMPLETE, SMI_ENABLE_LOCKED, SMI_RETURNED, SMM_RELOCATED, SMRAM_LOCKED,
};
use fstart_core::services::measure::kind::*;
use fstart_image_build::boot_test::{
    Asset, Bank, BootTest, BootTests, CRABEFI_FINISHED, GUEST_BOOTED, RunInput, Smp,
    TPM_REPORT_END, Tpm, TpmFamily,
};
extern crate std;
use std::{format, vec, vec::Vec};

use crate::messages::{PCI_ROOT_READY, READY};

impl<B: VirtBoardFacts> BootTests for super::Plan<B> {
    fn boot_tests() -> Vec<BootTest> {
        boot_tests(B::MACHINE)
    }
}

fn halt() -> BootTest {
    BootTest::new("halt", "halt").expect(READY)
}

fn linux(kernel: Asset) -> BootTest {
    BootTest::new("linux", "linux")
        .input(RunInput::Kernel(kernel))
        .expect(GUEST_BOOTED)
}

fn uefi() -> BootTest {
    BootTest::new("uefi", "uefi").expect(CRABEFI_FINISHED)
}

/// Multi-processor bring-up with SMM relocation on every CPU.
fn smm(name: &str, smp: Smp) -> BootTest {
    let cpus = smp.cpus;
    BootTest::new(name, "halt")
        .smp(smp)
        .expect(format!("{MP_COMPLETE} ({cpus} CPUs)"))
        .expect(format!("{cpus} {SMM_RELOCATED}"))
        .expect(SMRAM_LOCKED)
        .expect(SMI_ENABLE_LOCKED)
        .expect(format!("{SMI_RETURNED} {cpus} CPUs"))
        .expect(READY)
}

/// Measured boot: SeaBIOS (a coreboot payload) or CrabEFI continues the
/// event log fstart starts, and the guest's PCRs must replay from it.
fn measured(name: &str, payload: &str, family: TpmFamily, banks: &[Bank]) -> BootTest {
    let test = BootTest::new(name, payload)
        .expect(TPM_REPORT_END)
        .timeout(300);
    let mut events = vec![(EV_S_CRTM_VERSION, 0), (EV_S_CRTM_CONTENTS, 0)];
    let test = if payload == "coreboot" {
        // POST_CODE also describes SMM; EV_IPL proves SeaBIOS continued
        // measuring.
        events.push((
            match family {
                TpmFamily::Tpm12 => EV_POST_CODE,
                TpmFamily::Tpm20 => EV_POST_CODE2,
            },
            0,
        ));
        events.push((EV_IPL, 4));
        test.input(RunInput::Kernel(Asset::SeaBios))
            .input(RunInput::PayloadFile(
                "vgaroms/seavgabios.bin".into(),
                Asset::SeaVgaBios,
            ))
            .input(RunInput::PayloadFile(
                "etc/sercon-port".into(),
                Asset::SeaBiosSerconPort,
            ))
            .input(RunInput::Disk(Asset::BiosIso))
    } else {
        events.push((EV_EFI_BOOT_SERVICES_APPLICATION, 4));
        test.input(RunInput::Disk(Asset::UefiDisk))
    };
    test.tpm(Tpm {
        family,
        banks: banks.to_vec(),
        events,
    })
}

fn boot_tests(machine: VirtMachine) -> Vec<BootTest> {
    use Bank::*;
    use TpmFamily::*;
    match machine {
        VirtMachine::Q35 => vec![
            halt().expect(PCI_ROOT_READY),
            smm("halt-smp4", Smp::cpus(4)),
            smm("halt-smp4-multisocket", Smp::cpus(4).sockets(2, 2, 1)),
            smm("halt-smp4-hotplug", Smp::cpus(4).max_cpus(8)),
            uefi(),
            // The whole chain: fstart, CrabEFI, GRUB from the ESP, Linux.
            BootTest::new("uefi-disk", "uefi")
                .input(RunInput::Disk(Asset::UefiDisk))
                .expect(GUEST_BOOTED),
            measured("tpm12-seabios", "coreboot", Tpm12, &[Sha1]),
            measured("tpm20-seabios", "coreboot", Tpm20, &[Sha1, Sha256]),
            measured("tpm12-crabefi", "uefi", Tpm12, &[Sha1]),
            measured("tpm20-crabefi", "uefi", Tpm20, &[Sha1, Sha256]),
            measured("tpm20-crabefi-sha384", "uefi", Tpm20, &[Sha256, Sha384]),
        ],
        // 32-bit stages on a CPU without long mode (-cpu coreduo).
        VirtMachine::Q35ProtectedMode => vec![halt(), smm("halt-smp2", Smp::cpus(2))],
        VirtMachine::Riscv64 => vec![
            halt().expect(PCI_ROOT_READY),
            linux(Asset::Riscv64Linux).input(RunInput::Firmware(Asset::OpenSbi)),
            uefi().input(RunInput::Firmware(Asset::OpenSbi)),
        ],
        VirtMachine::SifiveU => vec![
            halt(),
            linux(Asset::Riscv64Linux).input(RunInput::Firmware(Asset::OpenSbi)),
        ],
        VirtMachine::Aarch64 => vec![
            halt().expect(PCI_ROOT_READY),
            linux(Asset::Aarch64Linux).input(RunInput::Firmware(Asset::Bl31)),
            uefi().input(RunInput::Firmware(Asset::Bl31)),
        ],
        VirtMachine::Armv7 => vec![halt().expect(PCI_ROOT_READY), linux(Asset::Armv7Linux)],
        // TF-A first: fstart is BL33 in pflash1, entered non-secure from BL31.
        VirtMachine::Sbsa => vec![
            halt()
                .expect(PCI_ROOT_READY)
                .input(RunInput::SecureFirmware(Asset::SbsaSecureFlash)),
        ],
    }
}
