#![no_std]
#![no_main]
fstart_platform_sifive::stage_bin!(
    fstart_board_sifive_unmatched::fu740::Fu740Program<fstart_board_sifive_unmatched::Board>
);
