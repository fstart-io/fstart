//! QEMU boot tests: how a platform's machine is booted for a test and what it
//! must print. Platforms write them in their host module, next to the plans
//! for the same machines, from the constants their stages log with; fbuild's
//! `boot-test` runs them.

use serde::{Deserialize, Serialize};

/// What every guest of the boot assets prints once its kernel reached
/// userspace (ci/boot-assets: init.c and x86_64-init).
pub const GUEST_BOOTED: &str = "FSTART_CI_BOOT_SUCCESS";
/// The x86_64 guest's last line after it printed the TPM's PCRs and event log.
pub const TPM_REPORT_END: &str = "TPMTEST_END";
/// What CrabEFI logs when its boot manager returns.
pub const CRABEFI_FINISHED: &str = "Boot manager finished";

/// A file of the boot assets (`nix build .#boot-assets`, ci/boot-assets).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Asset {
    Riscv64Linux,
    OpenSbi,
    Aarch64Linux,
    /// TF-A BL31 for the virt machine.
    Bl31,
    /// sbsa-ref's secure flash: TF-A BL1 and its FIP.
    SbsaSecureFlash,
    Armv7Linux,
    OrangePiR1Dtb,
    /// GPT disk with GRUB on an ESP, booting the x86_64 guest.
    UefiDisk,
    /// GRUB on an El Torito CD, booting the x86_64 guest.
    BiosIso,
    /// SeaBIOS as a coreboot payload, and the files it reads.
    SeaBios,
    SeaVgaBios,
    SeaBiosSerconPort,
}

impl Asset {
    /// The device tree of this name, if the boot assets have it.
    #[must_use]
    pub fn device_tree(name: &str) -> Option<Self> {
        [Self::OrangePiR1Dtb]
            .into_iter()
            .find(|asset| asset.path().rsplit('/').next() == Some(name))
    }

    /// Its path in the boot assets.
    #[must_use]
    pub const fn path(self) -> &'static str {
        match self {
            Self::Riscv64Linux => "riscv64/Image",
            Self::OpenSbi => "riscv64/fw_dynamic.bin",
            Self::Aarch64Linux => "aarch64/Image",
            Self::Bl31 => "aarch64/bl31.bin",
            Self::SbsaSecureFlash => "aarch64/sbsa-secure.pflash",
            Self::Armv7Linux => "armv7/zImage",
            Self::OrangePiR1Dtb => "armv7/sun8i-h2-plus-orangepi-r1.dtb",
            Self::UefiDisk => "x86_64/uefi.img",
            Self::BiosIso => "x86_64/bios.iso",
            Self::SeaBios => "seabios/bios.bin.elf",
            Self::SeaVgaBios => "seabios/vgabios.bin",
            Self::SeaBiosSerconPort => "seabios/sercon-port",
        }
    }
}

/// A run-time input of `fbuild run`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RunInput {
    Kernel(Asset),
    Firmware(Asset),
    SecureFirmware(Asset),
    Disk(Asset),
    /// `--payload-file NAME=asset`.
    PayloadFile(String, Asset),
}

impl RunInput {
    #[must_use]
    pub fn asset(&self) -> Asset {
        match self {
            Self::Kernel(asset)
            | Self::Firmware(asset)
            | Self::SecureFirmware(asset)
            | Self::Disk(asset)
            | Self::PayloadFile(_, asset) => *asset,
        }
    }
}

/// The guest's CPU topology.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Smp {
    pub cpus: u32,
    /// Sockets, cores per socket and threads per core.
    pub topology: Option<(u32, u32, u32)>,
    /// CPUs that can be hot-plugged up to.
    pub max_cpus: Option<u32>,
}

impl Smp {
    #[must_use]
    pub const fn cpus(cpus: u32) -> Self {
        Self {
            cpus,
            topology: None,
            max_cpus: None,
        }
    }

    #[must_use]
    pub const fn sockets(self, sockets: u32, cores: u32, threads: u32) -> Self {
        Self {
            topology: Some((sockets, cores, threads)),
            ..self
        }
    }

    #[must_use]
    pub const fn max_cpus(self, max_cpus: u32) -> Self {
        Self {
            max_cpus: Some(max_cpus),
            ..self
        }
    }

    /// QEMU's `-smp` value.
    #[must_use]
    pub fn qemu_arg(&self) -> String {
        let mut arg = self.cpus.to_string();
        if let Some((sockets, cores, threads)) = self.topology {
            arg += &format!(",sockets={sockets},cores={cores},threads={threads}");
        }
        if let Some(max) = self.max_cpus {
            arg += &format!(",maxcpus={max}");
        }
        arg
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TpmFamily {
    Tpm12,
    Tpm20,
}

/// A PCR bank.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Bank {
    Sha1,
    Sha256,
    Sha384,
    Sha512,
}

impl Bank {
    /// Its name, as swtpm and Linux spell it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Sha1 => "sha1",
            Self::Sha256 => "sha256",
            Self::Sha384 => "sha384",
            Self::Sha512 => "sha512",
        }
    }
}

/// Boot with a fresh software TPM; the guest's event log must replay to the
/// PCRs it reads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tpm {
    pub family: TpmFamily,
    /// The PCR banks the TPM allocates and the log must cover.
    pub banks: Vec<Bank>,
    /// Events the log must hold: (`measure::kind`, PCR).
    pub events: Vec<(u32, u32)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BootTest {
    /// Unique per board.
    pub name: String,
    /// The fbuild payload.
    pub payload: String,
    pub smp: Option<Smp>,
    pub inputs: Vec<RunInput>,
    /// Lines that must appear.
    pub expect: Vec<String>,
    /// Lines that must not.
    pub reject: Vec<String>,
    /// Boot deadline in seconds, when the runner's default is too short.
    pub timeout: Option<u64>,
    pub tpm: Option<Tpm>,
}

impl BootTest {
    #[must_use]
    pub fn new(name: &str, payload: &str) -> Self {
        Self {
            name: name.into(),
            payload: payload.into(),
            smp: None,
            inputs: Vec::new(),
            expect: Vec::new(),
            reject: Vec::new(),
            timeout: None,
            tpm: None,
        }
    }

    #[must_use]
    pub fn expect(mut self, line: impl Into<String>) -> Self {
        self.expect.push(line.into());
        self
    }

    #[must_use]
    pub fn reject(mut self, line: impl Into<String>) -> Self {
        self.reject.push(line.into());
        self
    }

    #[must_use]
    pub fn input(mut self, input: RunInput) -> Self {
        self.inputs.push(input);
        self
    }

    #[must_use]
    pub fn smp(mut self, smp: Smp) -> Self {
        self.smp = Some(smp);
        self
    }

    #[must_use]
    pub fn timeout(mut self, seconds: u64) -> Self {
        self.timeout = Some(seconds);
        self
    }

    #[must_use]
    pub fn tpm(mut self, tpm: Tpm) -> Self {
        self.tpm = Some(tpm);
        self
    }
}

/// The boot tests of a platform's plan for a board; none by default.
pub trait BootTests {
    fn boot_tests() -> Vec<BootTest> {
        Vec::new()
    }
}

/// The plan runner's `--boot-tests` answer.
pub fn emit<P: BootTests>() {
    println!(
        "{}",
        serde_json::to_string(&P::boot_tests()).expect("serialize boot tests")
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smp_renders_qemus_syntax() {
        assert_eq!(Smp::cpus(2).qemu_arg(), "2");
        assert_eq!(
            Smp::cpus(4).sockets(2, 2, 1).qemu_arg(),
            "4,sockets=2,cores=2,threads=1"
        );
        assert_eq!(Smp::cpus(4).max_cpus(8).qemu_arg(), "4,maxcpus=8");
    }
}
