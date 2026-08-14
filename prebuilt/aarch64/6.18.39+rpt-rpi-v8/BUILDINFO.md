# Prebuilt BSC target module

- Architecture: `aarch64`
- Builder: Raspberry Pi 4 Model B
- Target kernel: `6.18.39+rpt-rpi-v8`
- Module vermagic: `6.18.39+rpt-rpi-v8 SMP preempt mod_unload modversions aarch64`
- Source: `kernel/` from the same Git commit as these artifacts

The module is compatible only with a target whose `uname -r` and kernel
configuration match the values above. The Device Tree overlays are included so
`target-driver` can load the appropriate Pi 3 or Pi 4 target pin mapping at
runtime.

Run from the repository root:

```sh
test "$(uname -r)" = "6.18.39+rpt-rpi-v8"
sudo ./prebuilt/aarch64/target-driver \
  0x13 \
  ./prebuilt/aarch64/6.18.39+rpt-rpi-v8
```
