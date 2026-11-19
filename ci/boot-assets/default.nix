# What the QEMU boot tests boot besides fstart: Linux guests, the firmware
# that runs before or after fstart (OpenSBI, TF-A, SeaBIOS) and the disks
# GRUB boots from. Every input is pinned by flake.lock or a hash, so the
# assets are the same on CI and on a workstation:
#
#   nix build .#boot-assets    # then cargo fbuild boot-test --assets result
#
# The tree, as the boards' boot tests name it:
#
#   riscv64/Image, riscv64/fw_dynamic.bin
#   aarch64/Image, aarch64/bl31.bin, aarch64/sbsa-secure.pflash,
#     aarch64/bl31-sun50i-a64.bin, aarch64/sun50i-h5-orangepi-pc2.dtb
#   armv7/zImage, armv7/sun8i-h2-plus-orangepi-r1.dtb
#   x86_64/uefi.img (GRUB on an ESP), x86_64/bios.iso (GRUB El Torito)
#   seabios/bios.bin.elf, seabios/vgabios.bin, seabios/sercon-port
{ pkgs }:
let
  inherit (pkgs) lib;
  cross = pkgs.pkgsCross;
  linux = pkgs.linux_6_12;

  # The kernel's own initramfs builder: a cpio with device nodes needs no root.
  genInitCpio = pkgs.runCommandCC "gen_init_cpio" { } ''
    tar -xf ${linux.src} --wildcards --strip-components=2 '*/usr/gen_init_cpio.c'
    mkdir -p $out/bin
    cc -O2 -o $out/bin/gen_init_cpio gen_init_cpio.c
  '';

  # FSTART_CI_BOOT_SUCCESS init for an Arm or RISC-V guest (init.c).
  markerInit =
    target:
    pkgs.runCommand "fstart-ci-init-${target.stdenv.hostPlatform.config}"
      {
        nativeBuildInputs = [ target.stdenv.cc ];
        hardeningDisable = [ "all" ];
      }
      ''
        ${target.stdenv.cc.targetPrefix}cc -nostdlib -static -ffreestanding -Os -s \
          -Wl,-e,_start ${./init.c} -o $out
      '';

  # A tinyconfig kernel plus `fragment`, built for `target`. `initramfs` is a
  # gen_init_cpio list built into the kernel; `files` maps build outputs to
  # names in $out.
  tinyKernel =
    {
      name,
      target,
      arch,
      fragment,
      initramfs ? null,
      makeTargets,
      files,
    }:
    pkgs.stdenv.mkDerivation {
      pname = "fstart-ci-linux-${name}";
      inherit (linux) version src;
      depsBuildBuild = [ pkgs.buildPackages.stdenv.cc ];
      nativeBuildInputs = [
        target.stdenv.cc
      ]
      ++ (with pkgs; [
        bc
        bison
        elfutils
        flex
        openssl
        perl
        python3
      ]);
      hardeningDisable = [ "all" ];
      enableParallelBuilding = true;
      makeFlags = [
        "ARCH=${arch}"
        "CROSS_COMPILE=${target.stdenv.cc.targetPrefix}"
        "HOSTCC=cc"
      ];
      postPatch = "patchShebangs scripts";
      configurePhase = ''
        runHook preConfigure
        make $makeFlags tinyconfig
        cat ${fragment} >> .config
        ${lib.optionalString (initramfs != null) ''
          cat >> .config <<EOF
          CONFIG_INITRAMFS_SOURCE="${initramfs}"
          CONFIG_INITRAMFS_ROOT_UID=0
          CONFIG_INITRAMFS_ROOT_GID=0
          EOF
        ''}
        make $makeFlags olddefconfig
        runHook postConfigure
      '';
      buildFlags = makeTargets;
      installPhase = ''
        runHook preInstall
        mkdir -p $out
        ${lib.concatStrings (lib.mapAttrsToList (to: from: "cp ${from} $out/${to}\n") files)}
        runHook postInstall
      '';
    };

  markerInitramfs =
    target:
    pkgs.writeText "initramfs.list" ''
      dir /dev 0755 0 0
      nod /dev/console 0600 0 0 c 5 1
      nod /dev/kmsg 0600 0 0 c 1 11
      file /init ${markerInit target} 0755 0 0
    '';

  riscv64 = tinyKernel {
    name = "riscv64";
    target = cross.riscv64;
    arch = "riscv";
    fragment = ./kernel-riscv64.config;
    initramfs = markerInitramfs cross.riscv64;
    makeTargets = [ "Image" ];
    files.Image = "arch/riscv/boot/Image";
  };

  aarch64 = tinyKernel {
    name = "aarch64";
    target = cross.aarch64-multiplatform;
    arch = "arm64";
    fragment = ./kernel-aarch64.config;
    initramfs = markerInitramfs cross.aarch64-multiplatform;
    makeTargets = [
      "Image"
      "allwinner/sun50i-h5-orangepi-pc2.dtb"
    ];
    files = {
      Image = "arch/arm64/boot/Image";
      "sun50i-h5-orangepi-pc2.dtb" = "arch/arm64/boot/dts/allwinner/sun50i-h5-orangepi-pc2.dtb";
    };
  };

  armv7 = tinyKernel {
    name = "armv7";
    target = cross.armv7l-hf-multiplatform;
    arch = "arm";
    fragment = ./kernel-armv7.config;
    initramfs = markerInitramfs cross.armv7l-hf-multiplatform;
    makeTargets = [
      "zImage"
      "allwinner/sun8i-h2-plus-orangepi-r1.dtb"
    ];
    files = {
      zImage = "arch/arm/boot/zImage";
      "sun8i-h2-plus-orangepi-r1.dtb" = "arch/arm/boot/dts/allwinner/sun8i-h2-plus-orangepi-r1.dtb";
    };
  };

  x86_64 = tinyKernel {
    name = "x86_64";
    target = pkgs;
    arch = "x86";
    fragment = ./kernel-x86_64.config;
    makeTargets = [ "bzImage" ];
    files.bzImage = "arch/x86/boot/bzImage";
  };

  # Busybox and x86_64-init: boot marker, then the TPM's PCRs and event log.
  x86_64Initramfs =
    let
      busybox = pkgs.pkgsStatic.busybox;
      applets = [
        "sh"
        "mount"
        "cat"
        "echo"
        "base64"
        "dmesg"
        "grep"
        "poweroff"
        "sed"
      ];
      list = pkgs.writeText "initramfs.list" ''
        dir /dev 0755 0 0
        nod /dev/console 0600 0 0 c 5 1
        dir /bin 0755 0 0
        dir /proc 0755 0 0
        dir /sys 0755 0 0
        file /bin/busybox ${busybox}/bin/busybox 0755 0 0
        ${lib.concatMapStrings (applet: "slink /bin/${applet} busybox 0777 0 0\n") applets}
        file /init ${./x86_64-init} 0755 0 0
      '';
    in
    pkgs.runCommand "fstart-ci-x86_64-initramfs.cpio" { } ''
      ${genInitCpio}/bin/gen_init_cpio ${list} > $out
    '';

  # GRUB boots the guest; with its tpm module it also measures what it loads
  # (PCR 8 and 9), which the measured-boot tests check.
  grubCfg = tpm: ''
    set timeout=0
    ${lib.optionalString tpm "insmod tpm"}
    linux /vmlinuz console=ttyS0,115200 panic=-1 quiet
    initrd /initramfs.cpio
    boot
  '';

  # GPT disk with one FAT ESP: /EFI/BOOT/BOOTX64.EFI finds /vmlinuz and reads
  # /boot/grub/grub.cfg.
  uefiDisk =
    pkgs.runCommand "fstart-ci-uefi.img"
      {
        nativeBuildInputs = with pkgs; [
          dosfstools
          gptfdisk
          grub2_efi
          mtools
        ];
      }
      ''
        cat > embedded.cfg <<'EOF'
        search --no-floppy --file /vmlinuz --set=root
        configfile /boot/grub/grub.cfg
        EOF
        cat > grub.cfg <<'EOF'
        ${grubCfg true}
        EOF
        grub-mkstandalone -O x86_64-efi -o BOOTX64.EFI \
          --modules="part_gpt fat search configfile linux tpm" \
          "boot/grub/grub.cfg=embedded.cfg"

        # 64 MiB disk; the ESP spans sector 2048 to the backup GPT.
        start=2048 sectors=$((64 * 2048)) end=$((64 * 2048 - 34))
        truncate -s $((sectors * 512)) disk.img
        sgdisk --clear --new=1:$start:$end --typecode=1:ef00 --change-name=1:ESP disk.img
        truncate -s $(((end - start + 1) * 512)) esp.img
        mkfs.fat -F 32 -n ESP esp.img
        mmd -i esp.img ::/EFI ::/EFI/BOOT ::/boot ::/boot/grub
        mcopy -i esp.img BOOTX64.EFI ::/EFI/BOOT/BOOTX64.EFI
        mcopy -i esp.img grub.cfg ::/boot/grub/grub.cfg
        mcopy -i esp.img ${x86_64}/bzImage ::/vmlinuz
        mcopy -i esp.img ${x86_64Initramfs} ::/initramfs.cpio
        dd if=esp.img of=disk.img bs=512 seek=$start conv=notrunc status=none
        mv disk.img $out
      '';

  # GRUB (i386-pc) on an El Torito CD for SeaBIOS.
  biosIso =
    pkgs.runCommand "fstart-ci-bios.iso"
      {
        nativeBuildInputs = with pkgs; [
          grub2
          mtools
          xorriso
        ];
      }
      ''
        mkdir -p iso/boot/grub
        cp ${x86_64}/bzImage iso/vmlinuz
        cp ${x86_64Initramfs} iso/initramfs.cpio
        cat > iso/boot/grub/grub.cfg <<'EOF'
        ${grubCfg false}
        EOF
        grub-mkrescue -o $out iso
      '';

  # SeaBIOS as an fstart coreboot payload. The fork reads its files (the VGA
  # BIOS, etc/sercon-port) from the coreboot table instead of CBFS and
  # continues fstart's ACPI TPM event log. fbuild arguments:
  #
  #   --payload coreboot --kernel seabios/bios.bin.elf
  #   --payload-file vgaroms/seavgabios.bin=seabios/vgabios.bin
  #   --payload-file etc/sercon-port=seabios/sercon-port
  seabios = pkgs.stdenv.mkDerivation {
    pname = "seabios-fstart";
    version = "1.17.0-fstart";
    src = pkgs.fetchFromGitHub {
      owner = "fstart-io";
      repo = "seabios";
      rev = "44ed85f85fac03107009242c067fdcc01ce55546";
      hash = "sha256-QN5WW5ZbFBBaQLVJhUwPFmMBLNjAMd1wNG2R+xhMiGc=";
    };
    nativeBuildInputs = [ pkgs.python3 ];
    hardeningDisable = [
      "fortify"
      "pic"
      "stackprotector"
    ];
    enableParallelBuilding = true;
    configurePhase = ''
      runHook preConfigure
      cp ${./seabios.defconfig} .config
      make olddefconfig
      runHook postConfigure
    '';
    installPhase = ''
      runHook preInstall
      mkdir -p $out
      cp out/bios.bin.elf out/vgabios.bin $out/
      # SeaBIOS mirrors its text screen to this port: 0x3f8, a little-endian u64.
      printf '\370\003\000\000\000\000\000\000' > $out/sercon-port
      runHook postInstall
    '';
  };

  opensbi = cross.riscv64.opensbi;
  atf = cross.aarch64-multiplatform.buildArmTrustedFirmware;
  # BL31 for the virt machine, which fstart enters at EL2 with a GICv3.
  bl31Qemu = atf {
    platform = "qemu";
    extraMakeFlags = [
      "QEMU_USE_GIC_DRIVER=QEMU_GICV3"
      "bl31"
    ];
    filesToInstall = [ "build/qemu/release/bl31.bin" ];
  };
  # TF-A first on sbsa-ref: fstart is BL33 in pflash1, which TF-A's
  # PRELOADED_BL33_BASE default points at.
  tfaSbsa = atf {
    platform = "qemu_sbsa";
    extraMakeFlags = [
      "all"
      "fip"
    ];
    filesToInstall = [
      "build/qemu_sbsa/release/bl1.bin"
      "build/qemu_sbsa/release/fip.bin"
    ];
  };
  # Allwinner A64 and H5 share TF-A's sun50i_a64 platform (Orange Pi PC2).
  bl31Allwinner = cross.aarch64-multiplatform.armTrustedFirmwareAllwinner;
in
{
  inherit seabios;

  boot-assets = pkgs.runCommand "fstart-boot-assets" { } ''
    mkdir -p $out/{riscv64,aarch64,armv7,x86_64}
    ln -s ${riscv64}/Image $out/riscv64/Image
    ln -s ${opensbi}/share/opensbi/lp64/generic/firmware/fw_dynamic.bin $out/riscv64/
    ln -s ${aarch64}/* $out/aarch64/
    ln -s ${bl31Qemu}/bl31.bin $out/aarch64/bl31.bin
    ln -s ${bl31Allwinner}/bl31.bin $out/aarch64/bl31-sun50i-a64.bin
    # sbsa-ref's secure flash: BL1 at 0, the FIP at 0x12000, 256 MiB.
    truncate -s 256M $out/aarch64/sbsa-secure.pflash
    dd if=${tfaSbsa}/bl1.bin of=$out/aarch64/sbsa-secure.pflash conv=notrunc status=none
    dd if=${tfaSbsa}/fip.bin of=$out/aarch64/sbsa-secure.pflash bs=4096 seek=18 \
      conv=notrunc status=none
    ln -s ${armv7}/* $out/armv7/
    ln -s ${uefiDisk} $out/x86_64/uefi.img
    ln -s ${biosIso} $out/x86_64/bios.iso
    ln -s ${seabios} $out/seabios
  '';
}
