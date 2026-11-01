obj-m += bsc_rx_minimal.o
KERNEL_BUILD ?= /lib/modules/$(shell uname -r)/build
.PHONY: all modules controller clean
all: modules controller
modules:
	$(MAKE) -C $(KERNEL_BUILD) M=$(CURDIR) modules
controller: rx-write-controller
rx-write-controller: controller.c
	$(CC) -std=c11 -O2 -Wall -Wextra -Werror -o $@ $<
clean:
	$(MAKE) -C $(KERNEL_BUILD) M=$(CURDIR) clean
	$(RM) rx-write-controller
