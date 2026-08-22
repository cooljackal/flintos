<!-- SPDX-License-Identifier: Apache-2.0 -->

# RP2040 boot2 provenance

`src/boot2.rs` contains the 256-byte `.boot2` section from the measured Wio
RP2040 Mini first-light ELF. The probe used Pico SDK 2.1.1 at commit
`bddd20f928ce76142793bef434d4f75f4af6e433` and selected the SDK's
`bs2_default` W25Q080-compatible second stage.

Extract the reference bytes without converting or padding the ELF:

```text
arm-none-eabi-objcopy --dump-section .boot2=boot2.bin rp2040-first-light.elf
```

The expected SHA-256 is
`a1408dd2691089af701a1cad19530c539064608566633b9619d15b40d0357b1e`.
The RP2040 boot ROM CRC-32/MPEG-2 over bytes 0 through 251 is `0x7a4eb274`;
the same value is stored little-endian in the final four bytes.

The Rust unit test checks size and CRC. `arch/armv6m/rp2040.ld` requires the
block at `0x10000000`, requires vectors at `0x10000100`, and rejects any
boot2 whose linked size is not exactly 256 bytes.

The derived bytes retain the Pico SDK's BSD-3-Clause terms in
`LICENSE.PICO-SDK.txt`.
