# SeaBIOS as a coreboot payload

fstart boots coreboot payloads with `--payload coreboot`: it loads the ELF
given with `--kernel`, writes a coreboot table and enters the payload in
32-bit protected mode, like coreboot does.

There is no CBFS. Files coreboot would put there are handed over in RAM with
`--payload-file NAME=PATH`, one `0x46530001` coreboot table record each.
SeaBIOS needs a small change to read them, in
<https://github.com/fstart-io/seabios>; the boot tests build it at a pinned
revision:

```sh
nix build .#seabios
cargo fbuild assemble -b foxconn-d41s --release --payload coreboot \
  --kernel result/bios.bin.elf \
  --payload-file vgaroms/seavgabios.bin=result/vgabios.bin \
  --payload-file etc/sercon-port=result/sercon-port
```

The build hands SeaBIOS its VGA BIOS (SeaVGABIOS on the framebuffer from the
coreboot table) and `etc/sercon-port`, which mirrors the text screen to COM1.
