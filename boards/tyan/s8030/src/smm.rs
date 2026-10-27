//! The S8030's SMM image: the SP3 platform's OS-first FCH handler
//! (`fstart_platform_sp3::smm::Sp3SmmHandler`); the board adds no SMI
//! sources. fbuild's `smm` unit builds this library with
//! `fstart_stage_env = "smm"` and the platform's `bundle-smm` feature.

fstart_platform_sp3::smm_runtime::smm_bin!(crate::Board, fstart_platform_sp3::smm::Sp3SmmHandler);
