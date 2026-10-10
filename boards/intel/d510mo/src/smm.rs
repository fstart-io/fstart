//! D510MO uses the shared ICH7 APMC/ACPI, TCO and GPE SMM policy.

use crate::Board;
use fstart_driver_intel::southbridge::smi::IchSmmHandler;

fstart_platform_intel::smm::smm_bin!(Board, IchSmmHandler);
