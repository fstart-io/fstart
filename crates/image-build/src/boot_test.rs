//! QEMU boot tests: what a machine is booted with for a test and what it
//! must print. They are part of the machine's run recipe
//! ([`crate::run::RunRecipe::tests`]), which platforms write next to the
//! plans for the same machines from the constants their stages log with.
//! fbuild's `boot-test` launches each the way `fbuild qemu` launches the
//! recipe; a test adds only the files of its run and the lines to check.

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

/// A boot asset bound to a test's run, by the names `fbuild qemu` takes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Binding {
    /// The plan's or the run recipe's input of this name
    /// (`--input NAME=PATH`).
    Input(String, Asset),
    /// A file handed to a coreboot payload (`--payload-file NAME=PATH`).
    PayloadFile(String, Asset),
    /// The disk the recipe attaches (`--disk`). The guest boots a copy.
    Disk(Asset),
}

impl Binding {
    #[must_use]
    pub fn asset(&self) -> Asset {
        match self {
            Self::Input(_, asset) | Self::PayloadFile(_, asset) | Self::Disk(asset) => *asset,
        }
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
    pub const ALL: [Self; 4] = [Self::Sha1, Self::Sha256, Self::Sha384, Self::Sha512];

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

/// A fresh software TPM, attached as the recipe's TPM device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tpm {
    pub family: TpmFamily,
    /// The PCR banks a TPM 2.0 allocates.
    pub banks: Vec<Bank>,
}

/// Boot with a [`Tpm`]; the guest's event log must replay to the PCRs it
/// reads, cover exactly the TPM's banks and hold `events`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeasuredBoot {
    pub tpm: Tpm,
    /// Events the log must hold: (`measure::kind`, PCR).
    pub events: Vec<(u32, u32)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BootTest {
    /// Unique per board.
    pub name: String,
    /// The payload the plan of this test is selected with.
    pub payload: String,
    /// `-smp` for this run, verbatim like [`crate::run::RunRecipe::smp`].
    pub smp: Option<String>,
    pub bindings: Vec<Binding>,
    /// Lines that must appear.
    pub expect: Vec<String>,
    /// Lines that must not.
    pub reject: Vec<String>,
    /// Boot deadline in seconds, when the runner's default is too short.
    pub timeout: Option<u64>,
    pub measured: Option<MeasuredBoot>,
}

impl BootTest {
    #[must_use]
    pub fn new(name: &str, payload: &str) -> Self {
        Self {
            name: name.into(),
            payload: payload.into(),
            smp: None,
            bindings: Vec::new(),
            expect: Vec::new(),
            reject: Vec::new(),
            timeout: None,
            measured: None,
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

    /// Bind `asset` to the input `name`.
    #[must_use]
    pub fn input(mut self, name: &str, asset: Asset) -> Self {
        self.bindings.push(Binding::Input(name.into(), asset));
        self
    }

    #[must_use]
    pub fn payload_file(mut self, name: &str, asset: Asset) -> Self {
        self.bindings.push(Binding::PayloadFile(name.into(), asset));
        self
    }

    #[must_use]
    pub fn disk(mut self, asset: Asset) -> Self {
        self.bindings.push(Binding::Disk(asset));
        self
    }

    #[must_use]
    pub fn smp(mut self, smp: impl Into<String>) -> Self {
        self.smp = Some(smp.into());
        self
    }

    #[must_use]
    pub fn timeout(mut self, seconds: u64) -> Self {
        self.timeout = Some(seconds);
        self
    }

    #[must_use]
    pub fn measured(mut self, measured: MeasuredBoot) -> Self {
        self.measured = Some(measured);
        self
    }
}
