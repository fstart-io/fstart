//! QEMU machine recipes. Host tools prepare the declared boot medium and spawn
//! the process; machine arguments and geometry stay with platform policy.
extern crate std;
use super::{QemuLink, VirtMachine};
use fstart_core::{Platform, QemuMachine, RegionKind};
use std::{
    string::{String, ToString},
    vec::Vec,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootMedia {
    Bios,
    Pflash { bank_size: u64, secure_bank: bool },
    ArmVirt { bank_size: u64 },
    X86Pflash { bank_size: u64 },
    SunxiSd,
    Mtd,
}

#[derive(Debug)]
pub struct Launch {
    pub program: &'static str,
    /// Machine arguments, before the prepared boot medium and run-local devices.
    pub args: Vec<String>,
    pub media: BootMedia,
    pub allow_memory_override: bool,
    pub no_reboot: bool,
    pub pci_test: bool,
}

/// Resolve a declared machine (or the documented architecture default).
/// `kvm` and `smp` are run-local choices, not firmware build geometry.
pub fn resolve(
    platform: Platform,
    machine: Option<QemuMachine>,
    kvm: bool,
    smp: &str,
) -> Result<Launch, String> {
    let words = |args: &[&str]| args.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let mut launch = Launch {
        program: "qemu-system-arm",
        args: Vec::new(),
        media: BootMedia::Bios,
        allow_memory_override: true,
        no_reboot: true,
        pci_test: false,
    };
    match machine {
        Some(QemuMachine::SbsaRef) => {
            if platform != Platform::Aarch64 {
                return Err("QEMU sbsa-ref requires the aarch64 platform".into());
            }
            let policy = VirtMachine::Sbsa.qemu_policy()?;
            launch.program = "qemu-system-aarch64";
            launch.args = words(&["-M", "sbsa-ref", "-cpu", "max", "-nographic"]);
            launch.media = BootMedia::Pflash {
                bank_size: policy.flash_image.ok_or("SBSA flash aperture")?.size,
                secure_bank: true,
            };
            launch.allow_memory_override = false;
            launch.no_reboot = false;
            launch.pci_test = true;
        }
        Some(QemuMachine::SifiveU) => {
            if platform != Platform::Riscv64 {
                return Err("QEMU sifive_u requires the riscv64 platform".into());
            }
            let policy = VirtMachine::SifiveU.qemu_policy()?;
            let ram = policy
                .memory
                .iter()
                .find(|m| m.kind == RegionKind::Ram)
                .ok_or("sifive_u RAM")?;
            launch.program = "qemu-system-riscv64";
            launch.args = words(&[
                "-M",
                "sifive_u",
                "-m",
                &std::format!("{}M", ram.size >> 20),
                "-nographic",
            ]);
            launch.allow_memory_override = false;
            launch.no_reboot = false;
        }
        Some(QemuMachine::OrangePiPc) => {
            if platform != Platform::Armv7 {
                return Err("QEMU orangepi-pc requires armv7".into());
            }
            launch.args = words(&["-M", "orangepi-pc", "-nographic"]);
            launch.media = BootMedia::SunxiSd;
            launch.allow_memory_override = false;
            launch.no_reboot = false;
        }
        Some(QemuMachine::Ast2500Evb) => {
            if platform != Platform::Armv5te {
                return Err("QEMU ast2500-evb requires the armv5te platform".into());
            }
            launch.args = words(&[
                "-M",
                "ast2500-evb,execute-in-place=on,fmc-model=mt25ql512ab",
                "-nographic",
            ]);
            launch.media = BootMedia::Mtd;
        }
        None => match platform {
            Platform::Riscv64 => {
                let policy = VirtMachine::Riscv64.policy("halt")?;
                launch.program = "qemu-system-riscv64";
                launch.args = words(&["-machine", "virt", "-nographic", "-bios", "none"]);
                launch.media = BootMedia::Pflash {
                    bank_size: policy.flash.size,
                    secure_bank: false,
                };
                launch.pci_test = true;
            }
            Platform::Aarch64 | Platform::Armv7 => {
                let (machine, binary, model, cpu) = if platform == Platform::Aarch64 {
                    (
                        VirtMachine::Aarch64,
                        "qemu-system-aarch64",
                        "virt,secure=on,virtualization=on,gic-version=3",
                        "cortex-a72",
                    )
                } else {
                    (
                        VirtMachine::Armv7,
                        "qemu-system-arm",
                        "virt,highmem-ecam=off",
                        "cortex-a15",
                    )
                };
                launch.program = binary;
                launch.args = words(&["-machine", model, "-cpu", cpu, "-nographic"]);
                launch.media = BootMedia::ArmVirt {
                    bank_size: machine.policy("halt")?.flash.size / 2,
                };
                launch.pci_test = true;
            }
            Platform::Armv5te => {
                return Err("ARMv5TE has no QEMU virt machine; the board must name one".into());
            }
            Platform::X86 | Platform::X86_64 => {
                let machine = if platform == Platform::X86 {
                    VirtMachine::Q35ProtectedMode
                } else {
                    VirtMachine::Q35
                };
                let policy = machine.qemu_policy()?;
                let QemuLink::X86 { flash, .. } = policy.link else {
                    return Err("q35 flash aperture".into());
                };
                let ram_end = policy
                    .memory
                    .iter()
                    .filter(|m| m.kind == RegionKind::Ram)
                    .map(|m| m.base + m.size)
                    .max()
                    .ok_or("q35 RAM")?;
                launch.program = "qemu-system-x86_64";
                launch.args = words(&[
                    "-M",
                    "q35",
                    "-accel",
                    if kvm { "kvm" } else { "tcg" },
                    "-cpu",
                    if platform == Platform::X86 {
                        "coreduo"
                    } else if kvm {
                        "host"
                    } else {
                        "max"
                    },
                    "-m",
                    &std::format!("{}M", ram_end >> 20),
                    "-smp",
                    smp,
                    "-display",
                    "none",
                    "-chardev",
                    "stdio,id=char0,mux=on,signal=off",
                    "-serial",
                    "chardev:char0",
                    "-mon",
                    "chardev=char0,mode=readline",
                    "-vga",
                    "none",
                    "-device",
                    "bochs-display",
                ]);
                launch.media = BootMedia::X86Pflash {
                    bank_size: flash.size,
                };
            }
        },
    }
    Ok(launch)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flash_geometry_comes_from_the_build_policy() {
        assert_eq!(
            resolve(Platform::Riscv64, None, false, "1").unwrap().media,
            BootMedia::Pflash {
                bank_size: VirtMachine::Riscv64.policy("halt").unwrap().flash.size,
                secure_bank: false
            }
        );
        assert_eq!(
            resolve(Platform::Aarch64, None, false, "1").unwrap().media,
            BootMedia::ArmVirt {
                bank_size: VirtMachine::Aarch64.policy("halt").unwrap().flash.size / 2
            }
        );
        assert_eq!(
            resolve(Platform::Aarch64, Some(QemuMachine::SbsaRef), false, "1")
                .unwrap()
                .media,
            BootMedia::Pflash {
                bank_size: VirtMachine::Sbsa
                    .qemu_policy()
                    .unwrap()
                    .flash_image
                    .unwrap()
                    .size,
                secure_bank: true
            }
        );
        assert_eq!(
            resolve(Platform::X86_64, None, false, "4").unwrap().media,
            BootMedia::X86Pflash {
                bank_size: 16 << 20
            }
        );
    }

    #[test]
    fn machine_capabilities_do_not_depend_on_exclusion_ladders() {
        let sbsa = resolve(Platform::Aarch64, Some(QemuMachine::SbsaRef), false, "1").unwrap();
        assert!(!sbsa.allow_memory_override && !sbsa.no_reboot && sbsa.pci_test);
        let ast = resolve(Platform::Armv5te, Some(QemuMachine::Ast2500Evb), false, "1").unwrap();
        assert!(ast.allow_memory_override && ast.no_reboot && !ast.pci_test);
        assert!(resolve(Platform::X86_64, Some(QemuMachine::OrangePiPc), false, "1").is_err());
        assert!(resolve(Platform::Armv5te, None, false, "1").is_err());
        let x86 = resolve(Platform::X86, None, false, "4").unwrap();
        assert!(x86.args.windows(2).any(|w| w == ["-cpu", "coreduo"]));
        assert!(x86.args.windows(2).any(|w| w == ["-smp", "4"]));
    }
}
