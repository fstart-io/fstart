#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""Compare the Rust APCB editor with coreboot's Picasso v2 Python tool.
Usage: compare-apcb.py COREBOOT_ROOT RUST_APCB_BINARY
"""
from pathlib import Path
import subprocess
import sys
import tempfile
root = Path(sys.argv[1]).resolve()
rust = str(Path(sys.argv[2]).resolve())
with tempfile.TemporaryDirectory(prefix='fstart-apcb-') as tmp:
    tmp = Path(tmp)
    for size in [64,128,256,512]:
        template = root/f'src/mainboard/google/zork/uma_{size}.apcb'
        c, r = tmp/f'python{size}.apcb', tmp/f'rust{size}.apcb'
        spd = root/'spd/ddr4/set-0/spd-1.hex'
        subprocess.run(['python3',str(root/'util/apcb/apcb_edit.py'),str(template),str(c),
            '--hex','--strip_manufacturer_information','--spd_0_0',str(spd),
            '--board_id_gpio0','132','1','0','--board_id_gpio1','90','1','0',
            '--board_id_gpio2','86','3','0','--board_id_gpio3','69','1','0'],check=True)
        subprocess.run([rust,'--template',str(template),'--output',str(r),'--hex',
            '--strip-manufacturer','--spd-0-0',str(spd),'--gpio','132:1:0',
            '--gpio','90:1:0','--gpio','86:3:0','--gpio','69:1:0'],check=True)
        assert c.read_bytes() == r.read_bytes(), f'APCB UMA {size} differs'
        print(f'APCB UMA {size}: byte-identical ({r.stat().st_size} bytes)')
