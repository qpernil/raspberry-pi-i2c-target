# ARM64 Rust executables

These are convenience builds of the five crate-dependency-free Rust programs. They
contain no kernel module or Device Tree overlay.

They were built from clean Git commits on a Raspberry Pi 4 running Raspberry
Pi OS (Debian 13, ARM64), using Rust/Cargo 1.97.1 and glibc 2.41. Building on
Raspberry Pi OS provides a more conservative glibc baseline than the Ubuntu
development machines.

| File | Purpose | Source commit |
| --- | --- | --- |
| `controller` | FIFO-sized controller for the direct userspace demonstration | `caaf31524ccbf6c8cf9ec0151c61a6eccce822d2` |
| `controller-long` | Long-message controller for the kernel target driver | `caaf31524ccbf6c8cf9ec0151c61a6eccce822d2` |
| `target` | Direct `/dev/mem` FIFO-sized target demonstration | `caaf31524ccbf6c8cf9ec0151c61a6eccce822d2` |
| `target-driver` | Kernel module/overlay lifecycle, echo, and receive-only modes | `223f21cbd01eab5b3461b61327576dad61bf9cbc` |
| `virtual-display` | Independent SSD1306/SH1106 SDL viewer with default GPIO5/GPIO26 outputs | `1e2201f6481215ae54077def6af8941a113469d8` |

Verify the files before use:

```sh
(cd prebuilt/aarch64 && sha256sum -c SHA256SUMS)
```

GitHub `main` remains the source of truth. Ubuntu machines should normally build
the Rust programs locally with `cargo build --release --locked`. Every target
machine must build the C kernel module locally with `make -C kernel`.
