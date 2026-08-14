/* SPDX-License-Identifier: GPL-2.0 WITH Linux-syscall-note */
#ifndef _UAPI_BSC_TARGET_H
#define _UAPI_BSC_TARGET_H

#include <linux/ioctl.h>
#include <linux/types.h>

#define BSC_TARGET_ABI_VERSION 1
#define BSC_TARGET_IOC_MAGIC 'B'

struct bsc_target_info {
	__u32 abi_version;
	__u32 slave_address;
	__u32 fifo_size;
	__u32 max_transfer;
	__u32 poll_interval_ns;
	__u32 reserved[3];
};

struct bsc_target_stats {
	__u64 rx_transactions;
	__u64 rx_bytes;
	__u64 rx_overruns;
	__u64 rx_dropped;
	__u64 tx_transactions;
	__u64 tx_bytes;
	__u64 tx_underruns;
	__u64 tx_short_reads;
	__u64 interrupts;
	__u64 timer_runs;
};

#define BSC_TARGET_IOC_GET_INFO \
	_IOR(BSC_TARGET_IOC_MAGIC, 0x00, struct bsc_target_info)
#define BSC_TARGET_IOC_GET_STATS \
	_IOR(BSC_TARGET_IOC_MAGIC, 0x01, struct bsc_target_stats)
#define BSC_TARGET_IOC_CLEAR_STATS _IO(BSC_TARGET_IOC_MAGIC, 0x02)

#endif
