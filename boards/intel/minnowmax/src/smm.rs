//! MinnowBoard MAX uses the shared ICH APMC/ACPI, TCO and GPE SMM policy:
//! Bay Trail's ACPI I/O block is ICH compatible.

fstart_platform_intel::smm::smm_bin!(handler: fstart_driver_intel::southbridge::smi::handle);
