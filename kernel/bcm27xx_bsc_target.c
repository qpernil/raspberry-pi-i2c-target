// SPDX-License-Identifier: GPL-2.0-only
/*
 * Interrupt-driven I2C target driver for the BCM2835-family SPI/BSC target.
 *
 * The hardware has a 16-byte FIFO, no DMA, and no clock stretching. While the
 * character device is open, FIFO interrupts service continuous transfers and
 * a high-resolution timer catches sub-threshold tails and observes
 * RXBUSY/TXBUSY dropping at STOP. A closed device stays electrically idle.
 */

#include <linux/device.h>
#include <linux/fs.h>
#include <linux/hrtimer.h>
#include <linux/interrupt.h>
#include <linux/io.h>
#include <linux/kernel.h>
#include <linux/miscdevice.h>
#include <linux/module.h>
#include <linux/mutex.h>
#include <linux/of.h>
#include <linux/pinctrl/consumer.h>
#include <linux/platform_device.h>
#include <linux/poll.h>
#include <linux/slab.h>
#include <linux/spinlock.h>
#include <linux/uaccess.h>
#include <linux/wait.h>

#include "bsc_target_uapi.h"

#define DR 0x00
#define RSR 0x04
#define SLV 0x08
#define CR 0x0c
#define FR 0x10
#define IFLS 0x14
#define IMSC 0x18
#define MIS 0x20
#define ICR 0x24

#define RSR_OE BIT(0)
#define RSR_UE BIT(1)

#define CR_EN BIT(0)
#define CR_I2C BIT(2)
#define CR_TXE BIT(8)
#define CR_RXE BIT(9)
#define CR_I2C_TARGET (CR_EN | CR_I2C | CR_TXE | CR_RXE)

#define FR_TXBUSY BIT(0)
#define FR_RXFE BIT(1)
#define FR_TXFF BIT(2)
#define FR_RXFF BIT(3)
#define FR_TXFE BIT(4)
#define FR_RXBUSY BIT(5)

#define IRQ_RX BIT(0)
#define IRQ_TX BIT(1)
#define IRQ_BREAK BIT(2)
#define IRQ_OE BIT(3)
#define IRQ_BASE (IRQ_RX | IRQ_BREAK | IRQ_OE)
#define IRQ_ALL (IRQ_BASE | IRQ_TX)

#define IFLS_TX_HALF 2
#define IFLS_RX_HALF (2 << 3)
#define IFLS_HALF (IFLS_TX_HALF | IFLS_RX_HALF)

#define BSC_FIFO_SIZE 16
#define BSC_MAX_TRANSFER 8192
#define BSC_RX_SLOTS 1024
#define BSC_DEFAULT_POLL_NS 100000
#define BSC_MIN_POLL_NS 20000
#define BSC_MAX_POLL_NS 500000

struct bsc_rx_slot {
	u32 len;
	u8 data[BSC_MAX_TRANSFER];
};

struct bsc_target {
	struct device *dev;
	void __iomem *regs;
	int irq;
	u32 address;
	u32 poll_interval_ns;
	struct pinctrl *pinctrl;
	struct pinctrl_state *pins_active;
	struct pinctrl_state *pins_idle;

	spinlock_t lock;
	struct mutex state_lock;
	struct mutex read_lock;
	struct mutex write_lock;
	wait_queue_head_t rx_wait;
	wait_queue_head_t tx_wait;
	atomic_t opened;
	bool dead;
	bool active;

	u8 rx_work[BSC_MAX_TRANSFER];
	size_t rx_work_len;
	bool rx_overflowed;
	struct bsc_rx_slot *rx_slots;
	u8 rx_read[BSC_MAX_TRANSFER];
	u32 rx_head;
	u32 rx_tail;
	u32 rx_count;

	u8 tx_data[BSC_MAX_TRANSFER];
	size_t tx_len;
	size_t tx_loaded;
	bool tx_queued;

	struct bsc_target_stats stats;
	struct hrtimer timer;
	struct miscdevice miscdev;
};

static inline u32 bsc_read(struct bsc_target *bsc, u32 reg)
{
	return readl(bsc->regs + reg);
}

static inline void bsc_write(struct bsc_target *bsc, u32 reg, u32 value)
{
	writel(value, bsc->regs + reg);
}

static void bsc_set_interrupts_locked(struct bsc_target *bsc)
{
	u32 mask = IRQ_BASE;

	if (bsc->tx_queued && bsc->tx_loaded < bsc->tx_len)
		mask |= IRQ_TX;
	bsc_write(bsc, IMSC, mask);
}

static void bsc_configure_locked(struct bsc_target *bsc)
{
	bsc_write(bsc, IMSC, 0);
	bsc_write(bsc, CR, 0);
	bsc_write(bsc, RSR, 0);
	bsc_write(bsc, SLV, bsc->address);
	bsc_write(bsc, IFLS, IFLS_HALF);
	bsc_write(bsc, ICR, IRQ_ALL);
	bsc_write(bsc, CR, CR_I2C_TARGET);
	bsc_set_interrupts_locked(bsc);
}

static void bsc_reset_io_locked(struct bsc_target *bsc)
{
	bsc->rx_work_len = 0;
	bsc->rx_overflowed = false;
	bsc->rx_head = 0;
	bsc->rx_tail = 0;
	bsc->rx_count = 0;
	bsc->tx_len = 0;
	bsc->tx_loaded = 0;
	bsc->tx_queued = false;
}

static void bsc_disable_locked(struct bsc_target *bsc)
{
	bsc_write(bsc, IMSC, 0);
	bsc_write(bsc, CR, 0);
	bsc_write(bsc, RSR, 0);
	bsc_write(bsc, SLV, 0);
	bsc_reset_io_locked(bsc);
}

static void bsc_drain_rx_locked(struct bsc_target *bsc)
{
	while (!(bsc_read(bsc, FR) & FR_RXFE)) {
		u8 byte = bsc_read(bsc, DR);

		if (bsc->rx_work_len < BSC_MAX_TRANSFER)
			bsc->rx_work[bsc->rx_work_len++] = byte;
		else
			bsc->rx_overflowed = true;
	}
}

static void bsc_refill_tx_locked(struct bsc_target *bsc)
{
	while (bsc->tx_queued && bsc->tx_loaded < bsc->tx_len &&
	       !(bsc_read(bsc, FR) & FR_TXFF))
		bsc_write(bsc, DR, bsc->tx_data[bsc->tx_loaded++]);

	bsc_set_interrupts_locked(bsc);
}

static void bsc_finish_rx_locked(struct bsc_target *bsc)
{
	struct bsc_rx_slot *slot;

	if (!bsc->rx_work_len && !bsc->rx_overflowed)
		return;

	if (bsc->rx_overflowed) {
		bsc->stats.rx_overruns++;
	} else {
		if (bsc->rx_count == BSC_RX_SLOTS) {
			/* Keep the newest traffic when userspace temporarily falls behind. */
			bsc->rx_head = (bsc->rx_head + 1) % BSC_RX_SLOTS;
			bsc->rx_count--;
			bsc->stats.rx_dropped++;
		}
		slot = &bsc->rx_slots[bsc->rx_tail];
		slot->len = bsc->rx_work_len;
		memcpy(slot->data, bsc->rx_work, slot->len);
		bsc->rx_tail = (bsc->rx_tail + 1) % BSC_RX_SLOTS;
		bsc->rx_count++;
		bsc->stats.rx_transactions++;
		bsc->stats.rx_bytes += bsc->rx_work_len;
		wake_up_interruptible(&bsc->rx_wait);
	}

	bsc->rx_work_len = 0;
	bsc->rx_overflowed = false;
}

static void bsc_finish_tx_locked(struct bsc_target *bsc, size_t consumed)
{
	bool short_read = consumed < bsc->tx_len;

	bsc->stats.tx_transactions++;
	bsc->stats.tx_bytes += consumed;
	if (short_read)
		bsc->stats.tx_short_reads++;

	bsc->tx_queued = false;
	bsc->tx_len = 0;
	bsc->tx_loaded = 0;

	/* A short controller read leaves stale bytes in the hardware FIFO. */
	if (short_read)
		bsc_configure_locked(bsc);
	else
		bsc_set_interrupts_locked(bsc);

	wake_up_interruptible(&bsc->tx_wait);
}

static void bsc_service_locked(struct bsc_target *bsc)
{
	u32 status;
	u32 flags;
	bool rx_busy;

	status = bsc_read(bsc, RSR);
	if (status & RSR_OE)
		bsc->stats.rx_overruns++;
	if (status & RSR_UE)
		bsc->stats.tx_underruns++;
	if (status)
		bsc_write(bsc, RSR, 0);

	bsc_drain_rx_locked(bsc);
	if (bsc->tx_queued)
		bsc_refill_tx_locked(bsc);

	flags = bsc_read(bsc, FR);
	rx_busy = flags & FR_RXBUSY;

	if (!rx_busy && (bsc->rx_work_len || bsc->rx_overflowed))
		bsc_finish_rx_locked(bsc);

	if (!bsc->tx_queued)
		return;

	/*
	 * TXBUSY describes movement between the FIFO and the serializer, not a
	 * complete I2C controller transaction. In particular, it may clear
	 * between bytes. Resetting the peripheral on that transition discards
	 * the first queued byte or truncates a byte already being shifted.
	 *
	 * Once every response byte has entered the hardware and TXFE is set,
	 * release the software slot without resetting the peripheral. The final
	 * byte may still be in the serializer and must be allowed to finish.
	 */
	if (bsc->tx_loaded == bsc->tx_len && (flags & FR_TXFE))
		bsc_finish_tx_locked(bsc, bsc->tx_len);
}

static irqreturn_t bsc_irq(int irq, void *data)
{
	struct bsc_target *bsc = data;
	unsigned long flags;
	u32 pending;

	pending = bsc_read(bsc, MIS) & IRQ_ALL;
	if (!pending)
		return IRQ_NONE;

	spin_lock_irqsave(&bsc->lock, flags);
	if (!bsc->active) {
		spin_unlock_irqrestore(&bsc->lock, flags);
		return IRQ_NONE;
	}
	bsc->stats.interrupts++;
	bsc_service_locked(bsc);
	bsc_write(bsc, ICR, pending);
	spin_unlock_irqrestore(&bsc->lock, flags);

	return IRQ_HANDLED;
}

static enum hrtimer_restart bsc_timer(struct hrtimer *timer)
{
	struct bsc_target *bsc = container_of(timer, struct bsc_target, timer);
	unsigned long flags;

	spin_lock_irqsave(&bsc->lock, flags);
	if (bsc->dead || !bsc->active) {
		spin_unlock_irqrestore(&bsc->lock, flags);
		return HRTIMER_NORESTART;
	}
	bsc->stats.timer_runs++;
	bsc_service_locked(bsc);
	spin_unlock_irqrestore(&bsc->lock, flags);

	hrtimer_forward_now(timer, ns_to_ktime(bsc->poll_interval_ns));
	return HRTIMER_RESTART;
}

static int bsc_activate(struct bsc_target *bsc)
{
	unsigned long flags;
	int ret;

	ret = pinctrl_select_state(bsc->pinctrl, bsc->pins_active);
	if (ret)
		return ret;

	spin_lock_irqsave(&bsc->lock, flags);
	bsc_reset_io_locked(bsc);
	bsc->active = true;
	bsc_configure_locked(bsc);
	spin_unlock_irqrestore(&bsc->lock, flags);

	hrtimer_start(&bsc->timer, ns_to_ktime(bsc->poll_interval_ns),
		      HRTIMER_MODE_REL_PINNED);
	return 0;
}

static void bsc_deactivate(struct bsc_target *bsc)
{
	unsigned long flags;

	spin_lock_irqsave(&bsc->lock, flags);
	if (!bsc->active) {
		spin_unlock_irqrestore(&bsc->lock, flags);
		return;
	}
	bsc->active = false;
	bsc_disable_locked(bsc);
	spin_unlock_irqrestore(&bsc->lock, flags);
	hrtimer_cancel(&bsc->timer);
	pinctrl_select_state(bsc->pinctrl, bsc->pins_idle);
}

static int bsc_open(struct inode *inode, struct file *file)
{
	struct miscdevice *misc = file->private_data;
	struct bsc_target *bsc = container_of(misc, struct bsc_target, miscdev);
	int ret;

	if (atomic_cmpxchg(&bsc->opened, 0, 1))
		return -EBUSY;
	ret = nonseekable_open(inode, file);
	if (ret) {
		atomic_set(&bsc->opened, 0);
		return ret;
	}

	file->private_data = bsc;
	mutex_lock(&bsc->state_lock);
	if (bsc->dead)
		ret = -ENODEV;
	else
		ret = bsc_activate(bsc);
	mutex_unlock(&bsc->state_lock);
	if (ret)
		atomic_set(&bsc->opened, 0);
	return ret;
}

static int bsc_release(struct inode *inode, struct file *file)
{
	struct bsc_target *bsc = file->private_data;

	mutex_lock(&bsc->state_lock);
	if (!bsc->dead)
		bsc_deactivate(bsc);
	mutex_unlock(&bsc->state_lock);
	atomic_set(&bsc->opened, 0);
	return 0;
}

static ssize_t bsc_read_message(struct file *file, char __user *buffer,
				size_t count, loff_t *offset)
{
	struct bsc_target *bsc = file->private_data;
	struct bsc_rx_slot *slot;
	unsigned long flags;
	size_t len;
	int ret;

	if (mutex_lock_interruptible(&bsc->read_lock))
		return -ERESTARTSYS;

	for (;;) {
		if (READ_ONCE(bsc->dead)) {
			ret = -ENODEV;
			goto out_unlock;
		}
		if (READ_ONCE(bsc->rx_count))
			break;
		if (file->f_flags & O_NONBLOCK) {
			ret = -EAGAIN;
			goto out_unlock;
		}
		ret = wait_event_interruptible(bsc->rx_wait,
			READ_ONCE(bsc->rx_count) || READ_ONCE(bsc->dead));
		if (ret)
			goto out_unlock;
	}

	spin_lock_irqsave(&bsc->lock, flags);
	slot = &bsc->rx_slots[bsc->rx_head];
	len = slot->len;
	if (count < len) {
		spin_unlock_irqrestore(&bsc->lock, flags);
		ret = -EMSGSIZE;
		goto out_unlock;
	}
	memcpy(bsc->rx_read, slot->data, len);
	bsc->rx_head = (bsc->rx_head + 1) % BSC_RX_SLOTS;
	bsc->rx_count--;
	spin_unlock_irqrestore(&bsc->lock, flags);

	if (copy_to_user(buffer, bsc->rx_read, len)) {
		ret = -EFAULT;
		goto out_unlock;
	}
	ret = len;

out_unlock:
	mutex_unlock(&bsc->read_lock);
	return ret;
}

static ssize_t bsc_queue_response(struct file *file, const char __user *buffer,
				  size_t count, loff_t *offset)
{
	struct bsc_target *bsc = file->private_data;
	unsigned long flags;
	u8 *temporary;
	u32 hw_flags;
	int ret;

	if (!count || count > BSC_MAX_TRANSFER)
		return -EMSGSIZE;

	temporary = memdup_user(buffer, count);
	if (IS_ERR(temporary))
		return PTR_ERR(temporary);

	if (mutex_lock_interruptible(&bsc->write_lock)) {
		kfree(temporary);
		return -ERESTARTSYS;
	}

	for (;;) {
		if (READ_ONCE(bsc->dead)) {
			ret = -ENODEV;
			goto out;
		}
		if (!READ_ONCE(bsc->tx_queued))
			break;
		if (file->f_flags & O_NONBLOCK) {
			ret = -EAGAIN;
			goto out;
		}
		ret = wait_event_interruptible(bsc->tx_wait,
			!READ_ONCE(bsc->tx_queued) || READ_ONCE(bsc->dead));
		if (ret)
			goto out;
	}

	spin_lock_irqsave(&bsc->lock, flags);
	bsc_drain_rx_locked(bsc);
	hw_flags = bsc_read(bsc, FR);
	if (!(hw_flags & FR_RXBUSY) &&
	    (bsc->rx_work_len || bsc->rx_overflowed))
		bsc_finish_rx_locked(bsc);
	if ((hw_flags & (FR_RXBUSY | FR_TXBUSY)) || bsc->rx_work_len ||
	    bsc->rx_overflowed) {
		spin_unlock_irqrestore(&bsc->lock, flags);
		ret = -EBUSY;
		goto out;
	}

	memcpy(bsc->tx_data, temporary, count);
	bsc->tx_len = count;
	bsc->tx_loaded = 0;
	bsc->tx_queued = true;
	bsc_refill_tx_locked(bsc);
	spin_unlock_irqrestore(&bsc->lock, flags);
	ret = count;

out:
	mutex_unlock(&bsc->write_lock);
	kfree(temporary);
	return ret;
}

static __poll_t bsc_poll(struct file *file, poll_table *wait)
{
	struct bsc_target *bsc = file->private_data;
	__poll_t mask = 0;

	poll_wait(file, &bsc->rx_wait, wait);
	poll_wait(file, &bsc->tx_wait, wait);
	if (READ_ONCE(bsc->dead))
		return EPOLLERR | EPOLLHUP;
	if (READ_ONCE(bsc->rx_count))
		mask |= EPOLLIN | EPOLLRDNORM;
	if (!READ_ONCE(bsc->tx_queued))
		mask |= EPOLLOUT | EPOLLWRNORM;
	return mask;
}

static long bsc_ioctl(struct file *file, unsigned int command,
		      unsigned long argument)
{
	struct bsc_target *bsc = file->private_data;
	struct bsc_target_info info = {
		.abi_version = BSC_TARGET_ABI_VERSION,
		.slave_address = bsc->address,
		.fifo_size = BSC_FIFO_SIZE,
		.max_transfer = BSC_MAX_TRANSFER,
		.poll_interval_ns = bsc->poll_interval_ns,
	};
	struct bsc_target_stats stats;
	unsigned long flags;

	switch (command) {
	case BSC_TARGET_IOC_GET_INFO:
		return copy_to_user((void __user *)argument, &info, sizeof(info)) ?
			-EFAULT : 0;
	case BSC_TARGET_IOC_GET_STATS:
		spin_lock_irqsave(&bsc->lock, flags);
		stats = bsc->stats;
		spin_unlock_irqrestore(&bsc->lock, flags);
		return copy_to_user((void __user *)argument, &stats, sizeof(stats)) ?
			-EFAULT : 0;
	case BSC_TARGET_IOC_CLEAR_STATS:
		spin_lock_irqsave(&bsc->lock, flags);
		memset(&bsc->stats, 0, sizeof(bsc->stats));
		spin_unlock_irqrestore(&bsc->lock, flags);
		return 0;
	default:
		return -ENOTTY;
	}
}

static const struct file_operations bsc_fops = {
	.owner = THIS_MODULE,
	.open = bsc_open,
	.release = bsc_release,
	.read = bsc_read_message,
	.write = bsc_queue_response,
	.poll = bsc_poll,
	.unlocked_ioctl = bsc_ioctl,
	.compat_ioctl = bsc_ioctl,
};

static ssize_t stats_show(struct device *dev, struct device_attribute *attr,
			  char *buffer)
{
	struct bsc_target *bsc = dev_get_drvdata(dev);
	struct bsc_target_stats stats;
	unsigned long flags;

	spin_lock_irqsave(&bsc->lock, flags);
	stats = bsc->stats;
	spin_unlock_irqrestore(&bsc->lock, flags);

	return sysfs_emit(buffer,
		"rx_transactions=%llu rx_bytes=%llu rx_overruns=%llu "
		"rx_dropped=%llu tx_transactions=%llu tx_bytes=%llu "
		"tx_underruns=%llu tx_short_reads=%llu interrupts=%llu "
		"timer_runs=%llu\n",
		(unsigned long long)stats.rx_transactions,
		(unsigned long long)stats.rx_bytes,
		(unsigned long long)stats.rx_overruns,
		(unsigned long long)stats.rx_dropped,
		(unsigned long long)stats.tx_transactions,
		(unsigned long long)stats.tx_bytes,
		(unsigned long long)stats.tx_underruns,
		(unsigned long long)stats.tx_short_reads,
		(unsigned long long)stats.interrupts,
		(unsigned long long)stats.timer_runs);
}
static DEVICE_ATTR_RO(stats);

static void bsc_free_rx_slots(void *data)
{
	struct bsc_target *bsc = data;

	kvfree(bsc->rx_slots);
}

static int bsc_probe(struct platform_device *pdev)
{
	struct bsc_target *bsc;
	struct resource *resource;
	u32 address = 0x13;
	u32 poll_ns = BSC_DEFAULT_POLL_NS;
	int ret;

	bsc = devm_kzalloc(&pdev->dev, sizeof(*bsc), GFP_KERNEL);
	if (!bsc)
		return -ENOMEM;
	bsc->dev = &pdev->dev;
	bsc->rx_slots = kvcalloc(BSC_RX_SLOTS, sizeof(*bsc->rx_slots),
				 GFP_KERNEL);
	if (!bsc->rx_slots)
		return -ENOMEM;
	ret = devm_add_action_or_reset(&pdev->dev, bsc_free_rx_slots, bsc);
	if (ret)
		return ret;
	platform_set_drvdata(pdev, bsc);

	resource = platform_get_resource(pdev, IORESOURCE_MEM, 0);
	bsc->regs = devm_ioremap_resource(&pdev->dev, resource);
	if (IS_ERR(bsc->regs))
		return PTR_ERR(bsc->regs);

	bsc->irq = platform_get_irq(pdev, 0);
	if (bsc->irq < 0)
		return bsc->irq;

	of_property_read_u32(pdev->dev.of_node, "brcm,slave-address", &address);
	if (address < 0x08 || address > 0x77)
		return dev_err_probe(&pdev->dev, -EINVAL,
			"invalid non-reserved 7-bit address 0x%x\n", address);
	bsc->address = address;

	of_property_read_u32(pdev->dev.of_node, "brcm,poll-interval-ns", &poll_ns);
	if (poll_ns < BSC_MIN_POLL_NS || poll_ns > BSC_MAX_POLL_NS)
		return dev_err_probe(&pdev->dev, -EINVAL,
			"poll interval must be %u..%u ns\n",
			BSC_MIN_POLL_NS, BSC_MAX_POLL_NS);
	bsc->poll_interval_ns = poll_ns;

	bsc->pinctrl = devm_pinctrl_get(&pdev->dev);
	if (IS_ERR(bsc->pinctrl))
		return dev_err_probe(&pdev->dev, PTR_ERR(bsc->pinctrl),
			"cannot acquire pin control\n");
	bsc->pins_active = pinctrl_lookup_state(bsc->pinctrl, "active");
	if (IS_ERR(bsc->pins_active))
		return dev_err_probe(&pdev->dev, PTR_ERR(bsc->pins_active),
			"missing active pin state\n");
	bsc->pins_idle = pinctrl_lookup_state(bsc->pinctrl, "idle");
	if (IS_ERR(bsc->pins_idle))
		return dev_err_probe(&pdev->dev, PTR_ERR(bsc->pins_idle),
			"missing idle pin state\n");
	spin_lock_init(&bsc->lock);
	mutex_init(&bsc->state_lock);
	mutex_init(&bsc->read_lock);
	mutex_init(&bsc->write_lock);
	init_waitqueue_head(&bsc->rx_wait);
	init_waitqueue_head(&bsc->tx_wait);
	atomic_set(&bsc->opened, 0);
	hrtimer_setup(&bsc->timer, bsc_timer, CLOCK_MONOTONIC,
		      HRTIMER_MODE_REL_PINNED);
	bsc_disable_locked(bsc);

	ret = devm_request_irq(&pdev->dev, bsc->irq, bsc_irq, 0,
			       dev_name(&pdev->dev), bsc);
	if (ret)
		return ret;

	bsc->miscdev.minor = MISC_DYNAMIC_MINOR;
	bsc->miscdev.name = "bsc-target0";
	bsc->miscdev.fops = &bsc_fops;
	bsc->miscdev.parent = &pdev->dev;
	bsc->miscdev.mode = 0660;
	ret = misc_register(&bsc->miscdev);
	if (ret)
		return ret;

	ret = device_create_file(&pdev->dev, &dev_attr_stats);
	if (ret)
		goto unregister_misc;

	dev_info(&pdev->dev,
		 "inactive I2C target 0x%02x, IRQ %d, max transfer %u, RX slots %u, poll %u ns\n",
		 bsc->address, bsc->irq, BSC_MAX_TRANSFER, BSC_RX_SLOTS,
		 bsc->poll_interval_ns);
	return 0;

unregister_misc:
	misc_deregister(&bsc->miscdev);
	return ret;
}

static void bsc_remove(struct platform_device *pdev)
{
	struct bsc_target *bsc = platform_get_drvdata(pdev);

	mutex_lock(&bsc->state_lock);
	bsc->dead = true;
	bsc_deactivate(bsc);
	mutex_unlock(&bsc->state_lock);
	wake_up_interruptible(&bsc->rx_wait);
	wake_up_interruptible(&bsc->tx_wait);
	synchronize_irq(bsc->irq);

	device_remove_file(&pdev->dev, &dev_attr_stats);
	misc_deregister(&bsc->miscdev);
}

static const struct of_device_id bsc_of_match[] = {
	{ .compatible = "brcm,bcm27xx-bsc-target" },
	{ }
};
MODULE_DEVICE_TABLE(of, bsc_of_match);

static struct platform_driver bsc_driver = {
	.probe = bsc_probe,
	.remove = bsc_remove,
	.driver = {
		.name = "bcm27xx-bsc-target",
		.of_match_table = bsc_of_match,
	},
};
module_platform_driver(bsc_driver);

MODULE_AUTHOR("OpenAI Codex");
MODULE_DESCRIPTION("BCM27xx interrupt-driven SPI/BSC I2C target");
MODULE_LICENSE("GPL");
