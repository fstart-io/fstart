//! Host plan transport, shared by platform calculation and build orchestration.
use fstart_core::layout::{Region, RegionKind};
use fstart_core::{
    ConstVec, IntelIfdFlashLayout, IntelIfdRegion, IntelIfdRegionConfig, PayloadConfig,
    SecurityConfig, SmmConfig,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Span {
    pub base: u64,
    pub size: u64,
}
impl Span {
    pub fn end(self) -> Result<u64, String> {
        if self.size == 0 {
            return Err("empty layout range".into());
        }
        self.base
            .checked_add(self.size)
            .ok_or_else(|| "layout range overflows".into())
    }
    pub fn contains(self, base: u64, size: u64) -> bool {
        base >= self.base
            && base
                .checked_add(size)
                .zip(self.base.checked_add(self.size))
                .is_some_and(|(end, limit)| end <= limit)
    }
    pub fn overlaps(self, other: Self) -> bool {
        self.base < other.base + other.size && other.base < self.base + self.size
    }
    pub fn region(self, kind: RegionKind) -> Region {
        Region {
            kind,
            base: self.base,
            size: self.size,
        }
    }
}

/// Explicit consuming-compiler namespace and source path, not an ambient value.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompilerFileSelection {
    pub environment: String,
    pub path: String,
}
impl CompilerFileSelection {
    pub fn parse(value: &str) -> Result<Self, String> {
        let (environment, path) = value
            .split_once('=')
            .ok_or("compiler input must use ENV=PATH")?;
        let input = Self {
            environment: environment.into(),
            path: path.into(),
        };
        input.validate()?;
        Ok(input)
    }
    fn validate(&self) -> Result<(), String> {
        if !crate::build_plan::environment_name(&self.environment)
            || self.path.is_empty()
            || self.path.contains('\0')
        {
            return Err("invalid compiler input name or path".into());
        }
        Ok(())
    }
}

/// Explicit invocation choices. Defaults and supported combinations belong to
/// the selected platform, not the adapter or fbuild's family backend.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct BuildSelection {
    pub payload: Option<String>,
    /// A selected platform must bind every request, unchanged, to compiler units.
    #[serde(default)]
    pub compiler_files: Vec<CompilerFileSelection>,
    /// Platform-owned options by name (`fbuild --option NAME=VALUE`, plus
    /// the dedicated CLI flags that feed the well-known names below).
    /// A platform declares the names it consumes and rejects the rest
    /// ([`Self::reject_unknown_options`]); there is deliberately no typed
    /// field per platform knob in this shared transport.
    #[serde(default)]
    pub options: std::collections::BTreeMap<String, String>,
    /// The `package.metadata.fstart` tables of the board and of its platform
    /// crate, filled in by fbuild, for platform-owned firmware tables.
    #[serde(default)]
    pub manifests: PackageTables,
    /// SMBIOS Type 0 release date selected by fbuild, in MM/DD/YYYY form.
    #[serde(default)]
    pub smbios_release_date: Option<String>,
    /// Build-time log verbosity (`FSTART_LOG_LEVEL`) for every unit.
    #[serde(default)]
    pub log_level: Option<String>,
    /// Opt-in debug facilities (`fbuild --debug`), which the selected
    /// platform interprets; platforms without any reject them.
    #[serde(default)]
    pub debug: Vec<String>,
}

/// The `package.metadata.fstart` tables of the selected board and platform.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PackageTables {
    pub board: serde_json::Value,
    pub platform: serde_json::Value,
}

/// Well-known option names with dedicated fbuild flags. Path-valued:
/// `image-config` (explicit platform image configuration, e.g. a vendor
/// ROM), `blobs` (directory of the vendor firmware blobs the platform's
/// tables name) and `donor` (a vendor flash image fbuild extracts the
/// blob directory from; fbuild then sets `blobs`).
pub mod options {
    pub const IMAGE_CONFIG: &str = "image-config";
    pub const BLOBS: &str = "blobs";
    pub const DONOR: &str = "donor";
    pub const X86_LINUX_KERNEL_LOAD_ADDR: &str = "x86-linux-kernel-load-addr";
    pub const X86_LINUX_BOOTARGS: &str = "x86-linux-bootargs";
    pub const X86_LINUX_PRINT_MTRRS: &str = "x86-linux-print-mtrrs";
    /// The x86 Linux payload options, for `reject_unknown_options` lists.
    pub const X86_LINUX: &[&str] = &[
        X86_LINUX_KERNEL_LOAD_ADDR,
        X86_LINUX_BOOTARGS,
        X86_LINUX_PRINT_MTRRS,
    ];
    /// The vendor-image inputs an image format may bind.
    pub const VENDOR_IMAGE: &[&str] = &[IMAGE_CONFIG, BLOBS, DONOR];
}

/// The direct x86 Linux launcher options an x86 platform may offer.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct X86LinuxOptions {
    pub kernel_load_addr: Option<u64>,
    pub bootargs: Option<String>,
    pub print_mtrrs: bool,
}

impl X86LinuxOptions {
    #[must_use]
    pub fn any(&self) -> bool {
        self.kernel_load_addr.is_some() || self.bootargs.is_some() || self.print_mtrrs
    }
}

impl BuildSelection {
    pub fn validate_compiler_files(&self) -> Result<(), String> {
        for (name, value) in &self.options {
            if name.is_empty()
                || !name
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
                || value.contains('\0')
            {
                return Err(format!("invalid selection option {name:?}"));
            }
            if [options::IMAGE_CONFIG, options::BLOBS, options::DONOR].contains(&name.as_str())
                && value.is_empty()
            {
                return Err("invalid image configuration or blob directory path".into());
            }
        }
        let mut names = std::collections::BTreeSet::new();
        for input in &self.compiler_files {
            input.validate()?;
            if !names.insert(&input.environment) {
                return Err(format!("duplicate compiler input {}", input.environment));
            }
        }
        Ok(())
    }

    pub fn option(&self, name: &str) -> Option<&str> {
        self.options.get(name).map(String::as_str)
    }

    pub fn option_u64(&self, name: &str) -> Result<Option<u64>, String> {
        let Some(value) = self.option(name) else {
            return Ok(None);
        };
        let value = value.replace('_', "");
        let parsed = match value
            .strip_prefix("0x")
            .or_else(|| value.strip_prefix("0X"))
        {
            Some(hex) => u64::from_str_radix(hex, 16),
            None => value.parse(),
        };
        parsed.map(Some).map_err(|e| format!("option {name}: {e}"))
    }

    pub fn option_bool(&self, name: &str) -> Result<bool, String> {
        match self.option(name) {
            None => Ok(false),
            Some("true") => Ok(true),
            Some("false") => Ok(false),
            Some(other) => Err(format!(
                "option {name}: expected true or false, got {other:?}"
            )),
        }
    }

    pub fn set_option(&mut self, name: &str, value: impl Into<String>) {
        self.options.insert(name.to_string(), value.into());
    }

    /// The vendor-image path of `--image-config`, if bound.
    pub fn image_config(&self) -> Option<&str> {
        self.option(options::IMAGE_CONFIG)
    }

    /// The vendor blob directory of `--blobs`, if bound.
    pub fn blobs(&self) -> Option<&str> {
        self.option(options::BLOBS)
    }

    /// The parsed x86 Linux options (all absent by default).
    pub fn x86_linux(&self) -> Result<X86LinuxOptions, String> {
        Ok(X86LinuxOptions {
            kernel_load_addr: self.option_u64(options::X86_LINUX_KERNEL_LOAD_ADDR)?,
            bootargs: self.option(options::X86_LINUX_BOOTARGS).map(str::to_owned),
            print_mtrrs: self.option_bool(options::X86_LINUX_PRINT_MTRRS)?,
        })
    }

    /// Reject every option this platform does not declare. Platforms call
    /// this with their option names so a typo or a foreign platform's knob
    /// can never be silently dropped.
    pub fn reject_unknown_options(&self, allowed: &[&str]) -> Result<(), String> {
        for name in self.options.keys() {
            if !allowed.contains(&name.as_str()) {
                return Err(format!("this platform has no option '{name}'"));
            }
        }
        Ok(())
    }

    /// For platforms that offer no `--debug` facilities.
    pub fn reject_debug(&self) -> Result<(), String> {
        match self.debug.first() {
            Some(option) => Err(format!("this platform has no debug option '{option}'")),
            None => Ok(()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntelStagePlan {
    pub role: crate::intel_plan::IntelStage,
    /// Selected features relative to the platform dependency (qualified by Cargo's actual alias in fbuild).
    pub features: Vec<String>,
    /// How the FFS stores the stage for the stage that loads it.
    pub compression: fstart_core::Compression,
}

/// A compiled platform export, not a board-authoring schema.
pub type ResolvedPlan = crate::build_plan::BuildPlan;

/// Version of the transport between fbuild's generated plan runner and
/// a platform's host adapter. Bump it when [`BuildSelection`] or
/// [`ResolvedPlan`] change shape incompatibly; the runner and the
/// adapter are built from the same tree, so this catches stale cached
/// runners rather than negotiating between releases.
pub const PLAN_SCHEMA: u32 = 1;

/// What a host adapter prints on stdout: exactly one envelope, so a
/// platform rejection is a structured error for fbuild to surface, not
/// a panic backtrace on stderr. Diagnostics belong on stderr.
#[derive(Debug, Serialize, Deserialize)]
pub struct PlanEnvelope {
    pub schema: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<ResolvedPlan>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl PlanEnvelope {
    pub fn into_plan(self) -> Result<ResolvedPlan, String> {
        if self.schema != PLAN_SCHEMA {
            return Err(format!(
                "platform plan schema {} does not match fbuild's {PLAN_SCHEMA}; rebuild the plan runner",
                self.schema
            ));
        }
        match (self.plan, self.error) {
            (Some(plan), None) => Ok(plan),
            (None, Some(error)) => Err(error),
            _ => Err("platform plan envelope carries neither a plan nor an error".into()),
        }
    }
}

/// Serialize `resolve(selection)` (or the selection's own parse error)
/// as one [`PlanEnvelope`] JSON document.
pub fn emit_envelope(
    selection_json: &str,
    resolve: impl FnOnce(BuildSelection) -> Result<ResolvedPlan, String>,
) -> String {
    let envelope = match serde_json::from_str::<BuildSelection>(selection_json)
        .map_err(|e| format!("invalid build selection: {e}"))
        .and_then(resolve)
    {
        Ok(plan) => PlanEnvelope {
            schema: PLAN_SCHEMA,
            plan: Some(plan),
            error: None,
        },
        Err(error) => PlanEnvelope {
            schema: PLAN_SCHEMA,
            plan: None,
            error: Some(error),
        },
    };
    serde_json::to_string(&envelope).unwrap_or_else(|e| {
        format!("{{\"schema\":{PLAN_SCHEMA},\"error\":\"serialize platform plan: {e}\"}}")
    })
}

/// The contract fbuild's generated runner calls: a platform's
/// `Plan<B>` implements this for its board-fact type `B`, and the
/// generated `main` is `<Plan<B> as HostPlan<B>>::emit(argv[1])`.
/// Implementing `resolve` is the whole obligation — selection parsing,
/// error transport and serialization are shared.
pub trait HostPlan<B> {
    fn resolve(selection: BuildSelection) -> Result<ResolvedPlan, String>;

    fn emit(selection_json: &str) {
        std::println!("{}", emit_envelope(selection_json, Self::resolve));
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct IntelPlan {
    /// Platform-resolved mutable prefix; this transport does not know its formats.
    #[serde(default)]
    pub mutable_regions: fstart_core::board::MutableFlashRegions,
    pub reservations: crate::intel_plan::IntelReservations,
    /// Long mode or 32-bit protected mode; selects the target triple.
    pub platform: fstart_core::Platform,
    pub payload: String,
    pub stages: [IntelStagePlan; 3],
    pub smm_features: Vec<String>,
    pub flash: FlashTransport,
    pub max_cpus: u16,
    pub microcode: Vec<String>,
    /// Board-directory files packaged as verified `FileType::Data` assets.
    /// Their names are also the runtime lookup names.
    #[serde(default)]
    pub data_assets: Vec<String>,
    /// Assembler inputs for the selected payload, built by the platform
    /// policy: one place decides what the payload is and where it loads.
    #[serde(default)]
    pub payload_config: Option<PayloadConfig>,
    pub security: SecurityConfig,
    pub smm: SmmConfig,
}

/// Bounded active-region transport; authoring uses core's const-validated type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IfdTransport(pub Vec<(IntelIfdRegion, u32, u32)>);
impl From<IntelIfdFlashLayout> for IfdTransport {
    fn from(layout: IntelIfdFlashLayout) -> Self {
        Self(
            layout
                .regions
                .as_slice()
                .iter()
                .map(|r| (r.kind, r.offset, r.size))
                .collect(),
        )
    }
}
impl IfdTransport {
    pub fn decode(&self) -> Result<IntelIfdFlashLayout, String> {
        if self.0.is_empty() || self.0.len() > 8 {
            return Err("IFD needs 1..8 regions".into());
        }
        let mut regions = ConstVec::new(IntelIfdRegionConfig {
            kind: IntelIfdRegion::Descriptor,
            offset: 0,
            size: 0,
        });
        for &(kind, offset, size) in &self.0 {
            regions = regions.push(IntelIfdRegionConfig { kind, offset, size });
        }
        let layout = IntelIfdFlashLayout { regions };
        layout.check().map_err(str::to_owned)?;
        Ok(layout)
    }
}

/// Physical flash identity, without inventing descriptor regions on legacy hardware.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FlashTransport {
    IntelIfd(IfdTransport),
    X86Legacy(fstart_core::X86LegacyFlashLayout),
}

impl From<fstart_core::FlashLayout> for FlashTransport {
    fn from(layout: fstart_core::FlashLayout) -> Self {
        match layout {
            fstart_core::FlashLayout::IntelIfd(ifd) => Self::IntelIfd(ifd.into()),
            fstart_core::FlashLayout::X86Legacy(legacy) => Self::X86Legacy(legacy),
        }
    }
}

impl FlashTransport {
    pub fn decode(&self) -> Result<fstart_core::FlashLayout, String> {
        match self {
            Self::IntelIfd(ifd) => ifd.decode().map(fstart_core::FlashLayout::IntelIfd),
            Self::X86Legacy(legacy) if legacy.size.is_power_of_two() => {
                Ok(fstart_core::FlashLayout::X86Legacy(*legacy))
            }
            Self::X86Legacy(_) => {
                Err("legacy flash capacity must be a nonzero power of two".into())
            }
        }
    }

    pub fn windows(&self) -> Result<(Span, Span), String> {
        match self.decode()? {
            fstart_core::FlashLayout::IntelIfd(ifd) => Ok((
                Span {
                    base: ifd.base(),
                    size: u64::from(ifd.size()),
                },
                Span {
                    base: ifd.bios_base().ok_or("missing BIOS")?,
                    size: u64::from(ifd.bios_region().ok_or("missing BIOS")?.size),
                },
            )),
            fstart_core::FlashLayout::X86Legacy(legacy) => {
                let window = Span {
                    base: legacy.base(),
                    size: u64::from(legacy.size),
                };
                Ok((window, window))
            }
        }
    }
}

impl IntelPlan {
    /// Rust target the stages and the SMM handler are compiled for.
    pub fn target(&self) -> String {
        self.platform.target_triple().into()
    }

    pub fn validate(&self) -> Result<fstart_core::FlashLayout, String> {
        if !self.platform.is_x86() {
            return Err("Intel plans target x86".into());
        }
        if self.platform == fstart_core::Platform::X86 && self.payload == "uefi" {
            return Err("the UEFI payload requires long mode".into());
        }
        self.reservations.validate()?;
        let (flash, firmware) = self.flash.windows()?;
        if flash != self.reservations.flash || firmware != self.reservations.firmware {
            return Err("physical layout differs from resolved flash/firmware windows".into());
        }
        let bios_offset = match &self.flash {
            FlashTransport::IntelIfd(layout) => {
                layout
                    .decode()?
                    .bios_region()
                    .ok_or("missing BIOS region")?
                    .offset
            }
            FlashTransport::X86Legacy(_) => 0,
        };
        let prefix = u64::from(fstart_core::board::mutable_prefix_size(
            &self.mutable_regions,
            bios_offset,
        )?);
        if self.reservations.bootblock.image.base != firmware.base + prefix
            || firmware.size.checked_sub(prefix) != Some(self.reservations.bootblock.image.size)
        {
            return Err("mutable cache and reset image geometry differ".into());
        }
        if self.max_cpus == 0
            || self.smm.entry_points.is_some_and(|n| n < self.max_cpus)
            || self.smm.stack_size == 0
        {
            return Err("SMM entries/stacks must cover the MP CPU ceiling".into());
        }
        self.flash.decode()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explicit_compiler_selections_preserve_paths_and_reject_duplicates() {
        let file = CompilerFileSelection::parse("FSTART_BYTES=/a path/bytes=1.bin").unwrap();
        assert_eq!(file.path, "/a path/bytes=1.bin");
        for invalid in [
            "FILE",
            "=file",
            "FILE=",
            "PATH=file",
            "RUSTFLAGS=file",
            "1FILE=file",
            "FILE=bad\0path",
        ] {
            assert!(CompilerFileSelection::parse(invalid).is_err());
        }
        let mut selection = BuildSelection {
            compiler_files: vec![file.clone()],
            ..Default::default()
        };
        selection.validate_compiler_files().unwrap();
        selection.compiler_files.push(file);
        assert!(selection.validate_compiler_files().is_err());
    }

    #[test]
    fn imported_ifd_rejects_overlap_duplicate_overflow_and_bad_alignment() {
        let valid = || {
            IfdTransport(vec![
                (IntelIfdRegion::Descriptor, 0, 0x1000),
                (IntelIfdRegion::Gbe, 0x1000, 0x2000),
                (IntelIfdRegion::Me, 0x3000, 0x27d000),
                (IntelIfdRegion::Bios, 0x280000, 0x180000),
            ])
        };
        let layout = valid().decode().unwrap();
        assert_eq!(layout.size(), 0x400000);
        assert_eq!(layout.regions.len(), 4);
        assert_eq!(
            layout
                .regions
                .as_slice()
                .iter()
                .map(|r| r.kind)
                .collect::<Vec<_>>(),
            vec![
                IntelIfdRegion::Descriptor,
                IntelIfdRegion::Gbe,
                IntelIfdRegion::Me,
                IntelIfdRegion::Bios
            ]
        );
        assert_eq!(IfdTransport::from(layout).0, valid().0);
        let mut bad = valid();
        bad.0[0].1 = 0x1000;
        assert!(bad.decode().unwrap_err().contains("overlap"));
        let mut bad = valid();
        bad.0.push(bad.0[0]);
        assert!(bad.decode().unwrap_err().contains("duplicate"));
        let mut bad = valid();
        bad.0[1].1 = 0xfffff000;
        assert!(bad.decode().is_err());
        let mut bad = valid();
        bad.0[1].1 += 1;
        assert!(bad.decode().is_err());
    }
}
