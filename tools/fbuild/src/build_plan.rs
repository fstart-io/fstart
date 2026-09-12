use fstart_core::BoardConfig;
use fstart_core::acpi::AcpiExtraDevice;

#[derive(Debug, Clone)]
pub struct ParsedBoard {
    pub config: BoardConfig,
    pub acpi_only_devices: Vec<AcpiExtraDevice>,
    pub resolved: Option<crate::resolved_image::ResolvedImage>,
}
