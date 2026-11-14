//! How a plan's image runs under QEMU: host-only launch data the platform
//! states and the runner renders without per-machine branches. Machine,
//! CPU and device strings are QEMU's own namespace, passed through
//! verbatim. A recipe attaches artefacts, it does not compose them:
//! composing a flash image is an image step's job.
use serde::{Deserialize, Serialize};

/// A QEMU launch description owned by the platform/board: the machine,
/// CPU and memory defaults, how the built artefacts attach, and the
/// devices a normal boot carries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunRecipe {
    /// QEMU binary, e.g. `qemu-system-aarch64`.
    pub qemu: String,
    /// `-machine` value, properties included.
    pub machine: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu: Option<String>,
    /// Prefer KVM when the host offers it (`FSTART_QEMU_ACCEL` still
    /// overrides); a missing `cpu` then means `host` under KVM, `max`
    /// under TCG.
    #[serde(default)]
    pub accel_kvm: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<String>,
    /// vCPU count default (`-smp`); `FSTART_QEMU_SMP` overrides.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub smp: Option<String>,
    #[serde(default)]
    pub nographic: bool,
    pub media: Vec<RunMedia>,
    /// Verbatim `-device` values.
    #[serde(default)]
    pub devices: Vec<String>,
    /// Verbatim extra argv (console/display setup, `-no-reboot`, …).
    #[serde(default)]
    pub args: Vec<String>,
    /// How a caller-supplied `--disk` attaches, if at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk: Option<RunDisk>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum RunDisk {
    Ide,
    Nvme,
}

/// Which built artefact a medium carries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RunArtifact {
    /// The artefact of the plan's image step of this name.
    Step(String),
    /// The first stage's flat binary.
    Stage,
}

/// One firmware medium of a run. The transforms are QEMU-attachment
/// mechanics (padding to a flash bank, splitting banks, placing the
/// image on a card at the boot ROM's offset), performed by the runner
/// from these parameters — the sizes and offsets are the platform's
/// data, not runner constants.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RunMedia {
    Bios {
        artifact: RunArtifact,
    },
    Pflash {
        artifact: RunArtifact,
        unit: u32,
        size: u64,
        #[serde(default)]
        readonly: bool,
    },
    /// A caller-supplied complete pflash image, bound like the plan's
    /// inputs by `name` (`--input NAME=PATH`; e.g. TF-A for a secure
    /// bank), checked to be exactly `size`.
    ExternalPflash {
        name: String,
        unit: u32,
        size: u64,
    },
    /// Two pflash banks of `bank_size`: `-bios` when the artefact fits
    /// the first bank, otherwise split across units 0 and 1.
    SplitPflash {
        artifact: RunArtifact,
        bank_size: u64,
    },
    /// A zero-filled SD card of `size` with the artefact at `offset`
    /// (the Allwinner BROM's eGON offset).
    SdCard {
        artifact: RunArtifact,
        offset: u64,
        size: u64,
    },
    Mtd {
        artifact: RunArtifact,
    },
}

impl RunMedia {
    /// The built artefact the medium carries, if it carries one.
    pub fn artifact(&self) -> Option<&RunArtifact> {
        match self {
            Self::Bios { artifact }
            | Self::Pflash { artifact, .. }
            | Self::SplitPflash { artifact, .. }
            | Self::SdCard { artifact, .. }
            | Self::Mtd { artifact } => Some(artifact),
            Self::ExternalPflash { .. } => None,
        }
    }
}

impl RunRecipe {
    /// The names of the caller-supplied files the run takes.
    pub fn external_inputs(&self) -> impl Iterator<Item = &str> {
        self.media.iter().filter_map(|media| match media {
            RunMedia::ExternalPflash { name, .. } => Some(name.as_str()),
            _ => None,
        })
    }
}
