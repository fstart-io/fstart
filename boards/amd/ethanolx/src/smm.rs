//! OS-first SMM dispatch; no board-specific RAS or SEL transport.

fstart_platform_sp3::smm_runtime::smm_bin!(crate::Board, fstart_platform_sp3::smm::Sp3SmmHandler);
