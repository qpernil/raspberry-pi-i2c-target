// SPDX-License-Identifier: GPL-2.0-only
/* Exclusive diagnostic owner: configure once, receive only, retain raw bytes. */
#include <linux/device.h>
#include <linux/fs.h>
#include <linux/hrtimer.h>
#include <linux/interrupt.h>
#include <linux/io.h>
#include <linux/miscdevice.h>
#include <linux/module.h>
#include <linux/of.h>
#include <linux/pinctrl/consumer.h>
#include <linux/platform_device.h>
#include <linux/slab.h>
#include <linux/spinlock.h>
#include <linux/uaccess.h>

#define DR 0x00
#define RSR 0x04
#define SLV 0x08
#define CR 0x0c
#define FR 0x10
#define IFLS 0x14
#define IMSC 0x18
#define MIS 0x20
#define ICR 0x24
#define RXFE BIT(1)
#define RX_ONLY (BIT(0) | BIT(2) | BIT(9))
#define RX_IRQS (BIT(0) | BIT(3))
#define CAPACITY (32U * 1024U * 1024U)
#define POLL_NS 300000

struct rx_minimal {
	void __iomem *regs;
	struct pinctrl *pins;
	struct pinctrl_state *active_pins, *idle_pins;
	spinlock_t lock;
	struct hrtimer timer;
	struct miscdevice misc;
	u8 *bytes;
	size_t length;
	u64 received, dropped, overruns, underruns, irq_calls, timer_calls;
	u32 address;
	bool active;
};

/* No DR writes, READY operation, receive-completion logic or CR changes here. */
static void drain(struct rx_minimal *rx)
{
	u32 errors = readl(rx->regs + RSR);

	if (errors) {
		rx->overruns += !!(errors & BIT(0));
		rx->underruns += !!(errors & BIT(1));
		writel(0, rx->regs + RSR);
	}
	while (!(readl(rx->regs + FR) & RXFE)) {
		u8 byte = readl(rx->regs + DR);

		if (rx->length < CAPACITY)
			rx->bytes[rx->length++] = byte;
		else
			rx->dropped++;
		rx->received++;
	}
}

static irqreturn_t rx_irq(int irq, void *data)
{
	struct rx_minimal *rx = data;
	unsigned long flags;
	u32 pending = readl(rx->regs + MIS) & RX_IRQS;

	if (!pending)
		return IRQ_NONE;
	spin_lock_irqsave(&rx->lock, flags);
	if (rx->active) {
		rx->irq_calls++;
		drain(rx);
		writel(pending, rx->regs + ICR);
	}
	spin_unlock_irqrestore(&rx->lock, flags);
	return IRQ_HANDLED;
}

static enum hrtimer_restart rx_timer(struct hrtimer *timer)
{
	struct rx_minimal *rx = container_of(timer, struct rx_minimal, timer);
	unsigned long flags;

	spin_lock_irqsave(&rx->lock, flags);
	if (!rx->active) {
		spin_unlock_irqrestore(&rx->lock, flags);
		return HRTIMER_NORESTART;
	}
	rx->timer_calls++;
	drain(rx);
	spin_unlock_irqrestore(&rx->lock, flags);
	hrtimer_forward_now(timer, ns_to_ktime(POLL_NS));
	return HRTIMER_RESTART;
}

static ssize_t capture_read(struct file *file, char __user *buf, size_t count,
			    loff_t *pos)
{
	struct miscdevice *misc = file->private_data;
	struct rx_minimal *rx = container_of(misc, struct rx_minimal, misc);
	unsigned long flags;
	size_t length;

	/* Bytes below the published length are append-only for the module lifetime. */
	spin_lock_irqsave(&rx->lock, flags);
	length = rx->length;
	spin_unlock_irqrestore(&rx->lock, flags);
	return simple_read_from_buffer(buf, count, pos, rx->bytes, length);
}
static const struct file_operations capture_ops = {
	.owner = THIS_MODULE,
	.read = capture_read,
	.llseek = default_llseek,
};

static ssize_t stats_show(struct device *dev, struct device_attribute *attr,
			 char *buf)
{
	struct rx_minimal *rx = dev_get_drvdata(dev);
	unsigned long flags;
	ssize_t n;

	spin_lock_irqsave(&rx->lock, flags);
	n = sysfs_emit(buf,
		"bytes=%llu captured=%zu dropped=%llu overruns=%llu underruns=%llu irq_calls=%llu timer_calls=%llu CR=0x%x FR=0x%x RSR=0x%x SLV=0x%x\n",
		(unsigned long long)rx->received, rx->length,
		(unsigned long long)rx->dropped, (unsigned long long)rx->overruns,
		(unsigned long long)rx->underruns, (unsigned long long)rx->irq_calls,
		(unsigned long long)rx->timer_calls, readl(rx->regs + CR),
		readl(rx->regs + FR), readl(rx->regs + RSR), readl(rx->regs + SLV));
	spin_unlock_irqrestore(&rx->lock, flags);
	return n;
}
static DEVICE_ATTR_RO(stats);

static void free_capture(void *data)
{
	kvfree(data);
}

static int minimal_probe(struct platform_device *pdev)
{
	struct rx_minimal *rx;
	struct resource *res;
	int irq, ret;
	unsigned long flags;

	rx = devm_kzalloc(&pdev->dev, sizeof(*rx), GFP_KERNEL);
	if (!rx)
		return -ENOMEM;
	rx->bytes = kvzalloc(CAPACITY, GFP_KERNEL);
	if (!rx->bytes)
		return -ENOMEM;
	ret = devm_add_action_or_reset(&pdev->dev, free_capture, rx->bytes);
	if (ret)
		return ret;
	rx->address = 0x24;
	of_property_read_u32(pdev->dev.of_node, "brcm,slave-address", &rx->address);
	if (rx->address < 0x08 || rx->address > 0x77)
		return -EINVAL;
	res = platform_get_resource(pdev, IORESOURCE_MEM, 0);
	rx->regs = devm_ioremap_resource(&pdev->dev, res);
	if (IS_ERR(rx->regs))
		return PTR_ERR(rx->regs);
	rx->pins = devm_pinctrl_get(&pdev->dev);
	if (IS_ERR(rx->pins))
		return PTR_ERR(rx->pins);
	rx->active_pins = pinctrl_lookup_state(rx->pins, "active");
	rx->idle_pins = pinctrl_lookup_state(rx->pins, "idle");
	if (IS_ERR(rx->active_pins) || IS_ERR(rx->idle_pins))
		return -EINVAL;
	spin_lock_init(&rx->lock);
	hrtimer_setup(&rx->timer, rx_timer, CLOCK_MONOTONIC, HRTIMER_MODE_REL_PINNED);
	platform_set_drvdata(pdev, rx);
	irq = platform_get_irq(pdev, 0);
	if (irq < 0)
		return irq;
	ret = devm_request_irq(&pdev->dev, irq, rx_irq, 0, dev_name(&pdev->dev), rx);
	if (ret)
		return ret;
	rx->misc.minor = MISC_DYNAMIC_MINOR;
	rx->misc.name = "bsc-rx-minimal";
	rx->misc.fops = &capture_ops;
	rx->misc.parent = &pdev->dev;
	rx->misc.mode = 0400;
	ret = misc_register(&rx->misc);
	if (ret)
		return ret;
	ret = device_create_file(&pdev->dev, &dev_attr_stats);
	if (ret)
		goto deregister;
	ret = pinctrl_select_state(rx->pins, rx->active_pins);
	if (ret)
		goto remove_attr;
	spin_lock_irqsave(&rx->lock, flags);
	/* All control/address/threshold/mask programming is confined to startup. */
	writel(0, rx->regs + CR);
	writel(rx->address, rx->regs + SLV);
	writel(0, rx->regs + IFLS); /* RX one-eighth-full threshold. */
	writel(0, rx->regs + RSR);
	writel(0xf, rx->regs + ICR);
	writel(RX_IRQS, rx->regs + IMSC);
	rx->active = true;
	writel(RX_ONLY, rx->regs + CR);
	spin_unlock_irqrestore(&rx->lock, flags);
	hrtimer_start(&rx->timer, ns_to_ktime(POLL_NS), HRTIMER_MODE_REL_PINNED);
	dev_info(&pdev->dev, "receive-only diagnostic at 0x%02x, CR=0x205, no READY/TX path\n", rx->address);
	return 0;
remove_attr:
	device_remove_file(&pdev->dev, &dev_attr_stats);
deregister:
	misc_deregister(&rx->misc);
	return ret;
}

static void minimal_remove(struct platform_device *pdev)
{
	struct rx_minimal *rx = platform_get_drvdata(pdev);
	unsigned long flags;

	spin_lock_irqsave(&rx->lock, flags);
	rx->active = false;
	writel(0, rx->regs + IMSC);
	writel(0, rx->regs + CR);
	spin_unlock_irqrestore(&rx->lock, flags);
	hrtimer_cancel(&rx->timer);
	pinctrl_select_state(rx->pins, rx->idle_pins);
	device_remove_file(&pdev->dev, &dev_attr_stats);
	misc_deregister(&rx->misc);
}
static const struct of_device_id minimal_match[] = {
	{ .compatible = "brcm,bcm27xx-bsc-target" },
	{}
};
MODULE_DEVICE_TABLE(of, minimal_match);
static struct platform_driver minimal_driver = {
	.probe = minimal_probe,
	.remove = minimal_remove,
	.driver = { .name = "bsc-rx-minimal", .of_match_table = minimal_match },
};
module_platform_driver(minimal_driver);
MODULE_LICENSE("GPL");
MODULE_DESCRIPTION("Exclusive configure-once BSC receive-only diagnostic");
