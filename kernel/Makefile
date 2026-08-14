obj-m += bcm27xx_bsc_target.o

KERNEL_BUILD ?= /lib/modules/$(shell uname -r)/build
DTC ?= dtc

.PHONY: all modules overlays clean

all: modules overlays

modules:
	$(MAKE) -C $(KERNEL_BUILD) M=$(CURDIR) modules

overlays: bsc-target-pi3.dtbo bsc-target-pi4.dtbo

bsc-target-pi3.dtbo: bsc-target-pi3-overlay.dts
	$(DTC) -@ -I dts -O dtb -o $@ $<

bsc-target-pi4.dtbo: bsc-target-pi4-overlay.dts
	$(DTC) -@ -I dts -O dtb -o $@ $<

clean:
	$(MAKE) -C $(KERNEL_BUILD) M=$(CURDIR) clean
	$(RM) bsc-target-pi3.dtbo bsc-target-pi4.dtbo
