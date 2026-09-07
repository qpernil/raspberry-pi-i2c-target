# ARM64 Rust executables

These are convenience builds of the five crate-dependency-free Rust programs. They
contain no kernel module or Device Tree overlay.

They were built from the clean source commits listed below on
`ubuntu4` running Ubuntu 26.04.1 LTS (ARM64), using Rust/Cargo 1.98.1 and
glibc 2.43. ELF version inspection shows that `controller`, `controller-long`,
and `target` require at most `GLIBC_2.34`; `target-driver` and
`virtual-display` require at most `GLIBC_2.39`. This is compatible with the
Raspberry Pi OS Debian 13 targets using glibc 2.41.

| File | Purpose | Source commit |
| --- | --- | --- |
| `controller` | FIFO-sized controller for the direct userspace demonstration | `7b802cb4b414f2e32a59e4e289a197e42580d963` |
| `controller-long` | Long-message controller for the kernel target driver | `7b802cb4b414f2e32a59e4e289a197e42580d963` |
| `target` | Direct `/dev/mem` FIFO-sized target demonstration | `7b802cb4b414f2e32a59e4e289a197e42580d963` |
| `target-driver` | Kernel lifecycle, device profiles, READY response mode and receive-only mode | `e907455` |
| `virtual-display` | Independent SSD1306/SH1106 SDL viewer with default GPIO5/GPIO26 outputs | `7b802cb4b414f2e32a59e4e289a197e42580d963` |

Verify the files before use:

```sh
(cd prebuilt/aarch64 && sha256sum -c SHA256SUMS)
```

GitHub `main` remains the source of truth. Ubuntu machines should normally build
the Rust programs locally with `cargo build --release --locked`. Every target
machine must build the C kernel module locally with `make -C kernel`.
