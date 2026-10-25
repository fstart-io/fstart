//! Shared ICH7 APMC/ACPI, TCO and GPE policy.
use crate::Board;
use fstart_driver_intel::southbridge::smi::IchSmmHandler;
fstart_platform_intel::smm::smm_bin!(Board, IchSmmHandler);
