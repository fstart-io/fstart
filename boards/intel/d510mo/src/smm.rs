//! D510MO uses the shared ICH7 APMC/ACPI, TCO and GPE SMM policy.

fstart_platform_intel::smm::smm_bin!(handler: fstart_driver_intel::southbridge::smi::handle);
