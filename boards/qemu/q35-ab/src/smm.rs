//! QEMU q35 A/B SMM policy.
//!
//! Reuses the shared ICH handler (APMC/ACPI enable, TCO, GPE clear) for the
//! emulated ICH9 southbridge.

fstart_platform_qemu::smm::smm_bin!(handler: fstart_driver_intel::southbridge::smi::handle);
