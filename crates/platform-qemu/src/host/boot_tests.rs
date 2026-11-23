//! What each QEMU machine is booted with in its boot tests, and what it must
//! print: the lines its stages log (`crate::messages`, the arch's), and what
//! the guests of the boot assets print. They are part of the machine's run
//! recipe, so a test boots the machine as `fbuild qemu` does.

use crate::facts::VirtMachine;
use crate::messages::{PCI_ROOT_READY, RAMSTAGE_ENTERED, READY};
use fstart_arch::messages::{
    MP_COMPLETE, SMI_ENABLE_LOCKED, SMI_RETURNED, SMM_RELOCATED, SMRAM_LOCKED,
};
use fstart_core::services::measure::kind::*;
use fstart_image_build::{
    boot_test::{
        Asset, Bank, BootTest, CRABEFI_FINISHED, GUEST_BOOTED, MeasuredBoot, TPM_REPORT_END, Tpm,
        TpmFamily,
    },
    build_plan::FfsAssembly,
};
extern crate std;
use std::{format, vec, vec::Vec};

/// The name of sbsa-ref's secure flash, a run recipe input.
pub(super) const SECURE_FIRMWARE: &str = "secure-firmware";

fn halt() -> BootTest {
    BootTest::new("halt", "halt")
        .expect(RAMSTAGE_ENTERED)
        .expect(READY)
}

fn linux(kernel: Asset) -> BootTest {
    BootTest::new("linux", "linux")
        .input(FfsAssembly::KERNEL, kernel)
        .expect(GUEST_BOOTED)
}

fn uefi() -> BootTest {
    BootTest::new("uefi", "uefi").expect(CRABEFI_FINISHED)
}

/// Multi-processor bring-up with SMM relocation on `cpus` CPUs; `topology`
/// completes QEMU's `-smp` value.
fn smm(name: &str, cpus: u32, topology: &str) -> BootTest {
    BootTest::new(name, "halt")
        .smp(format!("{cpus}{topology}"))
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
        test.input(FfsAssembly::KERNEL, Asset::SeaBios)
            .payload_file("vgaroms/seavgabios.bin", Asset::SeaVgaBios)
            .payload_file("etc/sercon-port", Asset::SeaBiosSerconPort)
            .disk(Asset::BiosIso)
    } else {
        events.push((EV_EFI_BOOT_SERVICES_APPLICATION, 4));
        test.disk(Asset::UefiDisk)
    };
    test.measured(MeasuredBoot {
        tpm: Tpm {
            family,
            banks: banks.to_vec(),
        },
        events,
    })
}

pub(super) fn boot_tests(machine: VirtMachine) -> Vec<BootTest> {
    use Bank::*;
    use TpmFamily::*;
    match machine {
        VirtMachine::Q35 => vec![
            halt().expect(PCI_ROOT_READY),
            smm("halt-smp4", 4, ""),
            smm("halt-smp4-multisocket", 4, ",sockets=2,cores=2,threads=1"),
            smm("halt-smp4-hotplug", 4, ",maxcpus=8"),
            uefi(),
            // The whole chain: fstart, CrabEFI, GRUB from the ESP, Linux.
            BootTest::new("uefi-disk", "uefi")
                .disk(Asset::UefiDisk)
                .expect(GUEST_BOOTED),
            measured("tpm12-seabios", "coreboot", Tpm12, &[Sha1]),
            measured("tpm20-seabios", "coreboot", Tpm20, &[Sha1, Sha256]),
            measured("tpm12-crabefi", "uefi", Tpm12, &[Sha1]),
            measured("tpm20-crabefi", "uefi", Tpm20, &[Sha1, Sha256]),
            measured("tpm20-crabefi-sha384", "uefi", Tpm20, &[Sha256, Sha384]),
        ],
        // 32-bit stages on a CPU without long mode (-cpu coreduo).
        VirtMachine::Q35ProtectedMode => vec![halt(), smm("halt-smp2", 2, "")],
        // Two-stage RO/A/B over dual RO/RW pflash_cfi01 with swtpm + CrabEFI Capsule-on-Disk.
        VirtMachine::Q35Ab => {
            let tpm20 = || Tpm {
                family: Tpm20,
                banks: vec![Sha1, Sha256],
            };
            vec![
                halt()
                    .tpm(tpm20())
                    .expect("qemu-q35-ab ro: entered")
                    .expect("qemu-q35-ab ro: provisioned TPM rollback floor = 1")
                    .expect("qemu-q35-ab ro: TPM rollback floor locked")
                    .expect("qemu-q35-ab ro: selected slot A")
                    .expect(PCI_ROOT_READY),
                uefi()
                    .tpm(tpm20())
                    .expect("qemu-q35-ab ro: provisioned TPM rollback floor = 1")
                    .expect("qemu-q35-ab ro: TPM rollback floor locked")
                    .expect("qemu-q35-ab ro: selected slot A")
                    .expect(RAMSTAGE_ENTERED)
                    .expect("qemu-q35-ab ramstage: flash write-locked for normal boot")
                    .expect("Default keys enrolled and persisted successfully")
                    .expect("OsIndicationsSupported set: FW_UI=false, FMP=true, FileCapsule=true"),
                measured("tpm20-crabefi", "uefi", Tpm20, &[Sha1, Sha256])
                    .expect("qemu-q35-ab ro: provisioned TPM rollback floor = 1")
                    .expect("qemu-q35-ab ro: TPM rollback floor locked"),
            ]
        }
        VirtMachine::Riscv64 => vec![
            halt().expect(PCI_ROOT_READY),
            linux(Asset::Riscv64Linux).input(FfsAssembly::FIRMWARE, Asset::OpenSbi),
            uefi().input(FfsAssembly::FIRMWARE, Asset::OpenSbi),
        ],
        VirtMachine::SifiveU => vec![
            halt(),
            linux(Asset::Riscv64Linux).input(FfsAssembly::FIRMWARE, Asset::OpenSbi),
        ],
        VirtMachine::Aarch64 => vec![
            halt().expect(PCI_ROOT_READY),
            linux(Asset::Aarch64Linux).input(FfsAssembly::FIRMWARE, Asset::Bl31),
            uefi().input(FfsAssembly::FIRMWARE, Asset::Bl31),
        ],
        VirtMachine::Armv7 => vec![halt().expect(PCI_ROOT_READY), linux(Asset::Armv7Linux)],
        // TF-A first: fstart is BL33 in pflash1, entered non-secure from BL31.
        VirtMachine::Sbsa => vec![
            halt()
                .expect(PCI_ROOT_READY)
                .input(SECURE_FIRMWARE, Asset::SbsaSecureFlash),
        ],
    }
}
