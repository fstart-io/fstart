# SeaBIOS as a coreboot payload

fstart boots coreboot payloads with `--payload coreboot`: it loads the ELF
given with `--kernel`, writes a coreboot table and enters the payload in
32-bit protected mode, like coreboot does.

There is no CBFS. Files coreboot would put there are handed over in RAM with
`--payload-file NAME=PATH`, one `0x46530001` coreboot table record each.
SeaBIOS needs a small change to read them, on the `fstart-romfiles` branch
of <https://github.com/fstart-io/seabios>.

```sh
git clone -b fstart-romfiles https://github.com/fstart-io/seabios ../seabios-fstart
payloads/seabios/build.sh ../seabios-fstart   # prints the fbuild arguments
cargo fbuild assemble -b foxconn-d41s --release $(payloads/seabios/build.sh ../seabios-fstart)
```

The build hands SeaBIOS its VGA BIOS (SeaVGABIOS on the framebuffer from the
coreboot table) and `etc/sercon-port`, which mirrors the text screen to COM1.
