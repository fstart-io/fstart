//! Plain Rust board and build metadata builders.
//!
//! These builders are the typed Rust replacement path for parser-specific board
//! authoring. They intentionally describe facts only: hardware/runtime topology
//! in [`BoardInfo`] and host build/package policy in [`BuildInfo`]. The fixed
//! stage runner owns runtime control flow.

use heapless::{String as HString, Vec as HVec};
use serde::{Deserialize, Serialize};

use crate::{
    BusAddress, Compression, DeviceConfig, DeviceEdge, DeviceRole, DigestAlgorithm, FdtSource,
    MemoryMap, PayloadConfig, PayloadKind, Platform, SecurityConfig, SignatureAlgorithm,
    SocImageFormat,
};

/// Construct a bounded heapless string for static board metadata.
///
/// This keeps board crates from each defining their own `HString::try_from`
/// wrappers while still failing fast when a literal exceeds the schema capacity.
#[must_use]
pub fn hstr<const N: usize>(value: &str) -> HString<N> {
    HString::try_from(value).expect("string exceeds heapless capacity")
}

/// Construct a bounded heapless vector for static board metadata.
///
/// The output capacity `C` is inferred from the destination field type.
#[must_use]
pub fn hvec<T, const N: usize, const C: usize>(items: [T; N]) -> HVec<T, C> {
    let mut out = HVec::new();
    for item in items {
        out.push(item).ok().expect("heapless vec capacity");
    }
    out
}

/// Default x86 LinuxBoot payload policy used by simple PC-compatible boards.
#[must_use]
pub fn x86_linuxboot_payload() -> PayloadConfig {
    PayloadConfig {
        kind: PayloadKind::LinuxBoot,
        kernel_file: Some(hstr("bzImage")),
        kernel_load_addr: Some(0x0200_0000),
        fdt: FdtSource::Platform,
        dtb_addr: None,
        src_dtb_addr: None,
        bootargs: Some(hstr(
            "console=ttyS0,115200n8 earlycon=uart8250,io,0x3f8,115200n8 ignore_loglevel loglevel=8",
        )),
        print_x86_mtrrs: true,
        compression: Compression::Lz4,
        firmware: None,
        fit_file: None,
        fit_config: None,
        fit_parse: None,
    }
}

/// Default x86 UEFI payload policy used by PC-compatible boards.
#[must_use]
pub fn x86_uefi_payload() -> PayloadConfig {
    PayloadConfig {
        kind: PayloadKind::UefiPayload,
        kernel_file: None,
        kernel_load_addr: None,
        fdt: FdtSource::Platform,
        dtb_addr: None,
        src_dtb_addr: None,
        bootargs: None,
        print_x86_mtrrs: true,
        compression: Compression::Lz4,
        firmware: None,
        fit_file: None,
        fit_config: None,
        fit_parse: None,
    }
}

/// Generic board-device topology builder.
///
/// Platform and board crates should use this for flat [`DeviceConfig`] tables
/// instead of each carrying local `push`/capacity boilerplate. It deliberately
/// records topology facts only. Runtime driver bindings live separately and are
/// matched by device name by host codegen.
#[derive(Debug, Clone)]
pub struct DeviceTopology {
    devices: HVec<DeviceConfig, 32>,
}

impl DeviceTopology {
    /// Start an empty device topology.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            devices: HVec::new(),
        }
    }

    /// Add a root runtime device.
    #[must_use]
    pub fn root(self, name: &str) -> Self {
        self.device(name, None, None, DeviceRole::Runtime, true)
    }

    /// Add a child runtime device.
    #[must_use]
    pub fn child(self, parent: &str, name: &str, bus: BusAddress) -> Self {
        self.device(name, Some(parent), Some(bus), DeviceRole::Runtime, true)
    }

    /// Add a child runtime device with an explicit enabled policy.
    #[must_use]
    pub fn runtime_child(self, parent: &str, name: &str, bus: BusAddress, enabled: bool) -> Self {
        self.device(name, Some(parent), Some(bus), DeviceRole::Runtime, enabled)
    }

    /// Add a driverless child bus owned by its parent device.
    #[must_use]
    pub fn child_bus(self, parent: &str, name: &str, role: DeviceRole) -> Self {
        assert!(!role.is_runtime(), "child_bus requires a structural role");
        self.device(name, Some(parent), None, role, true)
    }

    /// Add a driverless child bus and declare its children in a scoped branch.
    #[must_use]
    pub fn bus<'a, F>(self, parent: &str, name: &'a str, role: DeviceRole, children: F) -> Self
    where
        F: FnOnce(DeviceBranch<'a>) -> DeviceBranch<'a>,
    {
        let topology = self.child_bus(parent, name, role);
        children(DeviceBranch {
            topology,
            parent: name,
        })
        .finish()
    }

    /// Add a driverless PCI/PCIe bridge/root-port node.
    #[must_use]
    pub fn pci_bridge(
        self,
        parent: &str,
        name: &str,
        device: u8,
        function: u8,
        enabled: bool,
    ) -> Self {
        self.device(
            name,
            Some(parent),
            Some(BusAddress::Pci(device, function)),
            DeviceRole::PciBridge,
            enabled,
        )
    }

    /// Finish as the bounded flat table consumed by existing board metadata.
    #[must_use]
    pub fn build(self) -> HVec<DeviceConfig, 32> {
        self.devices
    }

    fn device(
        mut self,
        name: &str,
        parent: Option<&str>,
        bus: Option<BusAddress>,
        role: DeviceRole,
        enabled: bool,
    ) -> Self {
        self.devices
            .push(DeviceConfig {
                name: hstr(name),
                parent: parent.map(hstr),
                bus,
                role,
                enabled,
            })
            .expect("device table capacity");
        self
    }
}

/// Scoped child builder returned by [`DeviceTopology::bus`].
#[derive(Debug, Clone)]
pub struct DeviceBranch<'a> {
    topology: DeviceTopology,
    parent: &'a str,
}

impl<'a> DeviceBranch<'a> {
    /// Add a runtime child to this branch's parent bus.
    #[must_use]
    pub fn child(self, name: &str, bus: BusAddress) -> Self {
        Self {
            topology: self.topology.child(self.parent, name, bus),
            parent: self.parent,
        }
    }

    /// Add a nested driverless bus below this branch.
    #[must_use]
    pub fn bus<'b, F>(self, name: &'b str, role: DeviceRole, children: F) -> DeviceBranch<'a>
    where
        F: FnOnce(DeviceBranch<'b>) -> DeviceBranch<'b>,
    {
        let topology = self.topology.bus(self.parent, name, role, children);
        DeviceBranch {
            topology,
            parent: self.parent,
        }
    }

    fn finish(self) -> DeviceTopology {
        self.topology
    }
}

impl Default for DeviceTopology {
    fn default() -> Self {
        Self::new()
    }
}

/// Dynamic-board blob ABI version emitted by [`Board::build_blob`].
pub const BOARD_BLOB_ABI_VERSION: u16 = 1;

/// Serialized dynamic-board facts for a generic stage.
///
/// This is the no-std, serde-friendly ABI skeleton used by the future dynamic
/// board-blob mode. Static typed mode can use [`BoardInfo`] directly; dynamic
/// mode serializes this wrapper and validates the ABI/platform before matching
/// devices to the compiled-in registry.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoardBlob {
    /// ABI version for runtime compatibility checks.
    pub abi_version: u16,
    /// Board topology/runtime facts.
    pub board: BoardInfo,
}

/// Runtime hardware facts produced by a Rust board crate.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoardInfo {
    /// Stable fstart board name.
    pub name: HString<64>,
    /// Target platform.
    pub platform: Platform,
    /// Memory map visible to firmware.
    pub memory: MemoryMap,
    /// Device declarations in deterministic declaration order.
    pub devices: HVec<DeviceConfig, 32>,
    /// Lowered parent/child topology edges from typed child builders.
    pub edges: HVec<DeviceEdge, 64>,
    /// Optional payload policy.
    pub payload: Option<PayloadConfig>,
}

/// Builder for [`BoardInfo`].
#[derive(Debug, Clone)]
pub struct Board {
    info: BoardInfo,
}

/// Convert full board configuration into runtime [`BoardInfo`].
#[must_use]
pub fn board_info_from_config(config: crate::BoardConfig) -> BoardInfo {
    let mut board = Board::new(config.name.as_str())
        .platform(config.platform)
        .memory(config.memory);
    for device in config.devices {
        board = board.device(device);
    }
    if let Some(payload) = config.payload {
        board = board.payload(payload);
    }
    board.build()
}

/// Common development signing policy for board metadata.
#[must_use]
pub fn dev_security_config(pubkey_file: &str) -> SecurityConfig {
    SecurityConfig {
        signing_algorithm: SignatureAlgorithm::Ed25519,
        pubkey_file: hstr(pubkey_file),
        required_digests: hvec([DigestAlgorithm::Sha256]),
    }
}

/// Derive the coarse flow profile from payload kind.
#[must_use]
pub fn flow_profile_from_config(config: &crate::BoardConfig) -> FlowProfile {
    match config.payload.as_ref().map(|payload| &payload.kind) {
        Some(PayloadKind::UefiPayload) => FlowProfile::Uefi,
        _ => FlowProfile::LinuxBoot,
    }
}

impl Board {
    /// Start a board metadata builder.
    #[must_use]
    pub fn new(name: &str) -> Self {
        let mut board_name = HString::new();
        board_name
            .push_str(name)
            .expect("board name exceeds BoardInfo capacity");
        Self {
            info: BoardInfo {
                name: board_name,
                platform: Platform::Riscv64,
                memory: MemoryMap {
                    regions: HVec::new(),
                    flash_layout: None,
                    car: None,
                },
                devices: HVec::new(),
                edges: HVec::new(),
                payload: None,
            },
        }
    }

    /// Set the target platform.
    #[must_use]
    pub const fn platform(mut self, platform: Platform) -> Self {
        self.info.platform = platform;
        self
    }

    /// Set the memory map.
    #[must_use]
    pub fn memory(mut self, memory: MemoryMap) -> Self {
        self.info.memory = memory;
        self
    }

    /// Append a device declaration.
    #[must_use]
    pub fn device(mut self, device: DeviceConfig) -> Self {
        self.info
            .devices
            .push(device)
            .expect("board has more than 32 device declarations");
        self
    }

    /// Append a lowered typed-topology edge.
    #[must_use]
    pub fn edge(mut self, edge: DeviceEdge) -> Self {
        self.info
            .edges
            .push(edge)
            .expect("board has more than 64 topology edges");
        self
    }

    /// Set payload policy.
    #[must_use]
    pub fn payload(mut self, payload: PayloadConfig) -> Self {
        self.info.payload = Some(payload);
        self
    }

    /// Finish the board metadata builder.
    #[must_use]
    pub fn build(self) -> BoardInfo {
        self.info
    }

    /// Finish the builder as a dynamic-board blob.
    #[must_use]
    pub fn build_blob(self) -> BoardBlob {
        BoardBlob {
            abi_version: BOARD_BLOB_ABI_VERSION,
            board: self.info,
        }
    }
}

/// Static typed versus dynamic board-blob policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BoardDataMode {
    /// Compile the board/device graph into the stage.
    StaticTyped,
    /// Attach a serialized board blob to a generic compiled stage.
    DynamicBlob,
}

/// Host packaging image policy.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageBuildInfo {
    /// Whether to emit a full flash image in addition to the FFS blob.
    pub full_flash_image: bool,
    /// SoC-specific boot image format.
    pub soc_image_format: SocImageFormat,
}

impl Default for ImageBuildInfo {
    fn default() -> Self {
        Self {
            full_flash_image: false,
            soc_image_format: SocImageFormat::None,
        }
    }
}

/// Per-stage host build metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StageBuildInfo {
    /// Stage name (`stage` for monolithic boards).
    pub name: HString<32>,
    /// Stage load address.
    pub load_addr: u64,
}

impl StageBuildInfo {
    /// Construct per-stage build metadata.
    #[must_use]
    pub fn new(name: &str, load_addr: u64) -> Self {
        let mut stage_name = HString::new();
        stage_name
            .push_str(name)
            .expect("stage name exceeds StageBuildInfo capacity");
        Self {
            name: stage_name,
            load_addr,
        }
    }
}

/// Host input file consumed during packaging.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PayloadInputInfo {
    /// Logical input name (`kernel`, `firmware`, `fit`, `microcode`, ...).
    pub name: HString<32>,
    /// Path relative to the board crate unless absolute.
    pub path: HString<128>,
}

impl PayloadInputInfo {
    /// Construct a payload/firmware input descriptor.
    #[must_use]
    pub fn new(name: &str, path: &str) -> Self {
        let mut input_name = HString::new();
        input_name
            .push_str(name)
            .expect("payload input name exceeds PayloadInputInfo capacity");
        let mut input_path = HString::new();
        input_path
            .push_str(path)
            .expect("payload input path exceeds PayloadInputInfo capacity");
        Self {
            name: input_name,
            path: input_path,
        }
    }
}

/// Build profile selected by board metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BuildProfile {
    /// Cargo dev profile.
    Dev,
    /// Cargo release profile.
    Release,
}

/// Coarse fixed-flow profile selected by board metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FlowProfile {
    /// Minimal stage flow for smoke-test and bring-up images.
    Minimal,
    /// LinuxBoot-style flow with payload load and handoff.
    LinuxBoot,
    /// UEFI payload flow.
    Uefi,
    /// Multi-stage firmware flow.
    MultiStage,
}

/// Host-side build and packaging facts produced by a Rust board crate.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildInfo {
    /// Stable fstart board name.
    pub name: HString<64>,
    /// Cargo package containing the board crate.
    pub board_package: HString<64>,
    /// Rust target triple.
    pub target: HString<64>,
    /// Preferred build profile.
    pub profile: BuildProfile,
    /// Coarse fixed-flow profile.
    pub flow_profile: FlowProfile,
    /// Static typed or dynamic board-blob build mode.
    pub board_data_mode: BoardDataMode,
    /// Per-stage build layout summary.
    pub stages: HVec<StageBuildInfo, 8>,
    /// Explicit board-selected cargo/backend features.
    pub features: HVec<HString<64>, 64>,
    /// Host image packaging policy.
    pub image: ImageBuildInfo,
    /// Host payload/firmware input files.
    pub payload_inputs: HVec<PayloadInputInfo, 16>,
}

/// Builder for [`BuildInfo`].
#[derive(Debug, Clone)]
pub struct Build {
    info: BuildInfo,
}

impl Build {
    /// Start a build metadata builder.
    #[must_use]
    pub fn new(name: &str) -> Self {
        let mut board_name = HString::new();
        board_name
            .push_str(name)
            .expect("board name exceeds BuildInfo capacity");
        Self {
            info: BuildInfo {
                name: board_name,
                board_package: HString::new(),
                target: HString::new(),
                profile: BuildProfile::Dev,
                flow_profile: FlowProfile::Minimal,
                board_data_mode: BoardDataMode::StaticTyped,
                stages: HVec::new(),
                features: HVec::new(),
                image: ImageBuildInfo::default(),
                payload_inputs: HVec::new(),
            },
        }
    }

    /// Start host build metadata from common board configuration fields.
    #[must_use]
    pub fn from_board_config(
        board_name: &str,
        board_package: &str,
        config: &crate::BoardConfig,
        profile: BuildProfile,
        flow_profile: FlowProfile,
    ) -> Self {
        Self::new(board_name)
            .board_package(board_package)
            .target(config.platform.target_triple())
            .profile(profile)
            .flow_profile(flow_profile)
            .image(ImageBuildInfo {
                full_flash_image: config.full_flash_image,
                soc_image_format: config.soc_image_format,
            })
            .feature(config.platform.as_str())
    }

    /// Set the Cargo board package name.
    #[must_use]
    pub fn board_package(mut self, package: &str) -> Self {
        self.info.board_package.clear();
        self.info
            .board_package
            .push_str(package)
            .expect("board package name exceeds BuildInfo capacity");
        self
    }

    /// Set the Rust target triple.
    #[must_use]
    pub fn target(mut self, target: &str) -> Self {
        self.info.target.clear();
        self.info
            .target
            .push_str(target)
            .expect("target triple exceeds BuildInfo capacity");
        self
    }

    /// Set the preferred build profile.
    #[must_use]
    pub const fn profile(mut self, profile: BuildProfile) -> Self {
        self.info.profile = profile;
        self
    }

    /// Set the coarse fixed-flow profile.
    #[must_use]
    pub const fn flow_profile(mut self, profile: FlowProfile) -> Self {
        self.info.flow_profile = profile;
        self
    }

    /// Select static typed or dynamic board-blob mode.
    #[must_use]
    pub const fn board_data_mode(mut self, mode: BoardDataMode) -> Self {
        self.info.board_data_mode = mode;
        self
    }

    /// Append a stage build descriptor.
    #[must_use]
    pub fn stage(mut self, stage: StageBuildInfo) -> Self {
        self.info
            .stages
            .push(stage)
            .expect("build info has more than 8 stages");
        self
    }

    /// Append an explicit Cargo/backend feature.
    #[must_use]
    pub fn feature(mut self, feature: &str) -> Self {
        let mut value = HString::new();
        value
            .push_str(feature)
            .expect("feature name exceeds BuildInfo capacity");
        self.info
            .features
            .push(value)
            .expect("build info has more than 64 features");
        self
    }

    /// Set image packaging policy.
    #[must_use]
    pub fn image(mut self, image: ImageBuildInfo) -> Self {
        self.info.image = image;
        self
    }

    /// Append a payload/firmware input descriptor.
    #[must_use]
    pub fn payload_input(mut self, input: PayloadInputInfo) -> Self {
        self.info
            .payload_inputs
            .push(input)
            .expect("build info has more than 16 payload inputs");
        self
    }

    /// Append payload and firmware input files declared by board payload policy.
    #[must_use]
    pub fn payload_inputs_from_config(mut self, config: &crate::BoardConfig) -> Self {
        if let Some(payload) = &config.payload {
            if let Some(kernel) = &payload.kernel_file {
                self = self.payload_input(PayloadInputInfo::new("kernel", kernel.as_str()));
            }
            if let Some(firmware) = &payload.firmware {
                self =
                    self.payload_input(PayloadInputInfo::new("firmware", firmware.file.as_str()));
            }
        }
        self
    }

    /// Finish the build metadata builder.
    #[must_use]
    pub fn build(self) -> BuildInfo {
        self.info
    }
}

#[cfg(test)]
mod tests {
    use heapless::String as HString;

    use super::{Board, Build, BuildProfile, FlowProfile};
    use crate::{io16, lpc_child, BusKind, BusPortId, DeviceConfig, DeviceEdge, DeviceRole};

    #[test]
    fn build_info_builder_records_host_metadata() {
        let info = Build::new("qemu-riscv64")
            .board_package("fstart-board-qemu-riscv64")
            .target("riscv64gc-unknown-none-elf")
            .profile(BuildProfile::Release)
            .flow_profile(FlowProfile::LinuxBoot)
            .build();

        assert_eq!(info.name.as_str(), "qemu-riscv64");
        assert_eq!(info.board_package.as_str(), "fstart-board-qemu-riscv64");
        assert_eq!(info.target.as_str(), "riscv64gc-unknown-none-elf");
        assert_eq!(info.profile, BuildProfile::Release);
        assert_eq!(info.flow_profile, FlowProfile::LinuxBoot);
    }

    #[test]
    #[should_panic(expected = "board name exceeds BoardInfo capacity")]
    fn board_builder_rejects_over_capacity_names() {
        let name = "x".repeat(65);
        let _ = Board::new(&name);
    }

    #[test]
    #[should_panic(expected = "board has more than 32 device declarations")]
    fn board_builder_rejects_over_capacity_device_lists() {
        let mut device_name = HString::new();
        device_name.push_str("dev").unwrap();
        let device = DeviceConfig {
            name: device_name,
            parent: None,
            bus: None,
            role: DeviceRole::Runtime,
            enabled: true,
        };

        let mut board = Board::new("capacity-test");
        for _ in 0..33 {
            board = board.device(device.clone());
        }
    }

    #[test]
    #[should_panic(expected = "target triple exceeds BuildInfo capacity")]
    fn build_info_builder_rejects_over_capacity_target_triples() {
        let target = "x".repeat(65);
        let _ = Build::new("capacity-test").target(&target);
    }

    #[test]
    #[should_panic(expected = "bus port name exceeds BusPortId capacity")]
    fn bus_port_id_rejects_over_capacity_names() {
        let name = "x".repeat(33);
        let _ = BusPortId::new(&name, BusKind::Lpc);
    }

    #[test]
    fn board_builder_records_typed_topology_edges() {
        let edge = lpc_child(0, 1, "lpc", io16(0x2e)).lower();
        let board = Board::new("topology-test").edge(edge).build();

        assert_eq!(board.edges.len(), 1);
        assert_eq!(board.edges[0].parent, 0);
        assert_eq!(board.edges[0].child, 1);
        assert_eq!(board.edges[0].port.kind, BusKind::Lpc);
        assert_eq!(board.edges[0].address, Some(crate::BusAddress::Lpc(0x2e)));
    }

    #[test]
    #[should_panic(expected = "board has more than 64 topology edges")]
    fn board_builder_rejects_over_capacity_topology_edges() {
        let edge = DeviceEdge {
            parent: 0,
            child: 1,
            port: BusPortId::new("lpc", BusKind::Lpc),
            address: Some(crate::BusAddress::Lpc(0x2e)),
        };
        let mut board = Board::new("capacity-test");
        for _ in 0..65 {
            board = board.edge(edge.clone());
        }
    }
}
