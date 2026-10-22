#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""Byte-for-byte differential test; no AMD binaries are needed or redistributed.
Usage: python3 compare-coreboot.py /path/to/coreboot-amdfwtool /path/to/fstart-amdfw
"""
from pathlib import Path
import subprocess
import sys
import tempfile

KEYS = [
    'AMD_PUBKEY_FILE', 'PSPBTLDR_FILE', 'PSP_SMUFW1_SUB1_FILE',
    'PSP_SMUFW1_SUB2_FILE', 'PSP_SMUFW2_SUB1_FILE', 'PSP_SMUFW2_SUB2_FILE',
    'PSPSECUREOS_FILE', 'PSP_ABL0_FILE', 'PSP_ABL1_FILE', 'PSP_ABL2_FILE',
    'PSP_ABL3_FILE', 'PSP_ABL4_FILE', 'PSP_ABL5_FILE', 'PSP_ABL6_FILE',
    'PSP_ABL7_FILE', 'PSP_IKEK_FILE', 'PSP_SECG1_FILE', 'PSP_SECG2_FILE',
    'PSP_MP2FW1_FILE', 'PSP_MP2FW2_FILE', 'PSP_MP2CFG_FILE', 'PSP_DRIVERS_FILE',
    'PSP_PMUI_FILE_SUB0_INS1', 'PSP_PMUI_FILE_SUB0_INS4',
    'PSP_PMUI_FILE_SUB1_INS1', 'PSP_PMUI_FILE_SUB1_INS4',
    'PSP_PMUD_FILE_SUB0_INS1', 'PSP_PMUD_FILE_SUB0_INS4',
    'PSP_PMUD_FILE_SUB1_INS1', 'PSP_PMUD_FILE_SUB1_INS4',
]


def compare(c, rust):
    with tempfile.TemporaryDirectory(prefix='fstart-amdfw-') as tmp:
        root = Path(tmp)
        lines = [f'FIRMWARE_LOCATION {root}', 'SOC_NAME Picasso']
        for index, key in enumerate(KEYS):
            name = f'blob{index}.bin'
            (root / name).write_bytes(bytes([index + 1]) * (257 + index * 37))
            lines.append(f'{key} {name}')
        (root / 'fw.cfg').write_text('\n'.join(lines)+'\n')
        (root / 'reset.bin').write_bytes(bytes(range(256)) * 512)
        (root / 'apcb.bin').write_bytes(b'APCB' + bytes([0x55]) * 4093)
        for index, (co, ro) in enumerate([
            ([], []),
            (['--use-pspsecureos','--load-mp2-fw'], ['--secure-os','--mp2']),
            (['--sharedmem','2011000','--sharedmem-size','8000',
              '--apob-nv-base','200000','--apob-nv-size','1000',
              '--nvram-base','ff202000','--nvram-size','1000'],
             ['--shared-memory','0x2011000','--shared-memory-size','0x8000',
              '--apob-nv-offset','0x200000','--apob-nv-size','0x1000',
              '--psp-nv-offset','0x202000','--psp-nv-size','0x1000']),
        ]):
            cf, rf = root/f'c{index}.bin', root/f'rust{index}.bin'
            common = ['--config',str(root/'fw.cfg')]
            subprocess.run([c,*common,'--flashsize','1000000','--location','e20000',
                '--output',str(cf),'--spi-read-mode','4','--spi-speed','0','--spi-micron-flag','0',
                '--soft-fuse','0x30000000','--apcb',str(root/'apcb.bin'),
                '--apob-base','2001000','--bios-bin',str(root/'reset.bin'),
                '--bios-bin-dest','2030000','--bios-uncomp-size','20000','--bios-bin-uncomp',*co],check=True)
            subprocess.run([rust,*common,'--source-root',str(root),'--flash-size','0x1000000',
                '--efs-offset','0xe20000','--partition-end','0x1000000','--output',str(rf),
                '--soft-fuse','0x30000000','--apcb',str(root/'apcb.bin'),
                '--reset-image',str(root/'reset.bin'),'--reset-destination','0x2030000',*ro],check=True)
            a,b = cf.read_bytes(),rf.read_bytes()
            if a != b:
                first = next((i for i,(x,y) in enumerate(zip(a,b)) if x != y),min(len(a),len(b)))
                import struct
                psp = struct.unpack_from('<I', a, 0x14)[0] - 0xff000000 - 0xe20000
                for entry in range(struct.unpack_from('<I', a, psp+8)[0]):
                    offset = psp+16+entry*16
                    ca, ra = a[offset:offset+16], b[offset:offset+16]
                    if ca != ra: print(f'PSP entry {entry}: C={ca.hex()} Rust={ra.hex()}')
                start = struct.unpack_from('<I', a, 0x1c)[0] - 0xff000000 - 0xe20000
                count = struct.unpack_from('<I', a, start+8)[0]
                for entry in range(count):
                    offset = start+16+entry*24
                    ca, ra = a[offset:offset+24], b[offset:offset+24]
                    if ca != ra: print(f'BIOS entry {entry}: C={ca.hex()} Rust={ra.hex()}')
                raise AssertionError(f'case {index}: first difference offset {first:#x}; lengths {len(a):#x}/{len(b):#x}; C={a[first:first+16].hex()} Rust={b[first:first+16].hex()}')
            print(f'case {index}: byte-identical ({len(a):#x} bytes)')


if __name__ == '__main__':
    compare(str(Path(sys.argv[1]).resolve()),str(Path(sys.argv[2]).resolve()))
