#![no_std]
#![no_main]
fstart_platform_intel::stage_bin!(
    fstart_platform_intel::i945::Program<fstart_board_intel_d945gclf::Board>
);
