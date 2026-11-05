/*
 * Realtek RTL8169 / RTL8111 (RTL8168) Gigabit Ethernet, as seen by a driver.
 *
 * Written for testing JustOS drivers. It follows the register set and the
 * descriptor rules of the real chips (Realtek datasheets, and the Linux
 * r8169 driver for the behaviour the datasheets leave out) and is strict:
 * whatever the real chip would ignore or get wrong is ignored here too and
 * reported with LOG_GUEST_ERROR (-d guest_errors), so a driver that only
 * works on a lenient model shows up.
 *
 * Two models:
 *   rtl8169  10ec:8169, PCI: BAR0 I/O, BAR1 memory; no MSI. Like the early
 *            RTL8169 revisions, TxConfig and RxConfig only take writes while
 *            the transmitter and receiver are on.
 *   rtl8168  10ec:8168 (RTL8111E-VL): BAR0 I/O, BAR2 64-bit memory, BAR4
 *            MSI-X table; MSI and MSI-X, edge-triggered.
 *   rtl8125  10ec:8125 (RTL8125B, 2.5 Gbit): like the rtl8168, but the
 *            interrupt mask and status are 32-bit registers at 0x38 and
 *            0x3C and the transmit poll is at 0x90 (bit 0, normal queue);
 *            the old poll and interrupt registers do nothing there.
 *
 * Not modelled: checksum and segmentation offload, VLAN tags, the high
 * priority queue, tally counters, power management, interrupt mitigation.
 *
 * This work is licensed under the terms of the GNU GPL, version 2 or later.
 */

#include "qemu/osdep.h"
#include <zlib.h>
#include "hw/pci/pci_device.h"
#include "hw/pci/msi.h"
#include "hw/pci/msix.h"
#include "hw/qdev-properties.h"
#include "net/net.h"
#include "net/eth.h"
#include "qemu/log.h"
#include "qemu/module.h"
#include "qapi/error.h"
#include "sysemu/dma.h"
#include "sysemu/sysemu.h"
#include "qom/object.h"

#define TYPE_RTL8169_BASE "rtl8169-base"
OBJECT_DECLARE_SIMPLE_TYPE(RTL8169State, RTL8169_BASE)

/* Registers */
enum {
    IDR0 = 0x00, MAR0 = 0x08, TNPDS = 0x20, THPDS = 0x28,
    CR = 0x37, TPPOLL = 0x38, IMR = 0x3C, ISR = 0x3E,
    TCR = 0x40, RCR = 0x44, MPC = 0x4C, CFG9346 = 0x50,
    CONFIG0 = 0x51, CONFIG5 = 0x56, PHYAR = 0x60, PHYSTATUS = 0x6C,
    RMS = 0xDA, CPCR = 0xE0, INTRMIT = 0xE2, RDSAR = 0xE4, MTPS = 0xEC,
    /* RTL8125 */
    IMR8125 = 0x38, ISR8125 = 0x3C, TPPOLL8125 = 0x90,
};

#define CR_RST  0x10
#define CR_RE   0x08
#define CR_TE   0x04
#define POLL_NPQ 0x40
#define POLL_HPQ 0x80

/* Interrupt status */
#define INT_ROK     0x0001
#define INT_RER     0x0002
#define INT_TOK     0x0004
#define INT_TER     0x0008
#define INT_RDU     0x0010
#define INT_LINKCHG 0x0020
#define INT_FOVW    0x0040
#define INT_TDU     0x0080

/* RCR accept bits */
#define RCR_AAP 0x01
#define RCR_APM 0x02
#define RCR_AM  0x04
#define RCR_AB  0x08

/* Descriptor bits */
#define D_OWN (1u << 31)
#define D_EOR (1u << 30)
#define D_FS  (1u << 29)
#define D_LS  (1u << 28)
#define RX_MAR (1u << 27)
#define RX_PAM (1u << 26)
#define RX_BAR (1u << 25)

/* The chips walk at most 1024 descriptors before wrapping. */
#define RING_MAX 1024
#define MAX_FRAME 16384

struct RTL8169State {
    PCIDevice parent_obj;
    MemoryRegion io;
    MemoryRegion mmio;
    NICState *nic;
    NICConf conf;

    bool is_8168;
    bool is_8125;
    uint8_t regs[256];
    uint16_t phy[32];
    uint32_t rx_index;
    uint32_t tx_index;
    bool irq_level;

    uint8_t frame[MAX_FRAME];
};

#define GERR(...) qemu_log_mask(LOG_GUEST_ERROR, "rtl8169: " __VA_ARGS__)

static uint16_t reg16(RTL8169State *s, int off)
{
    return lduw_le_p(&s->regs[off]);
}

static uint32_t reg32(RTL8169State *s, int off)
{
    return ldl_le_p(&s->regs[off]);
}

static uint64_t reg64(RTL8169State *s, int off)
{
    return ldq_le_p(&s->regs[off]);
}

static uint32_t isr(RTL8169State *s)
{
    return s->is_8125 ? reg32(s, ISR8125) : reg16(s, ISR);
}

static uint32_t imr(RTL8169State *s)
{
    return s->is_8125 ? reg32(s, IMR8125) : reg16(s, IMR);
}

static bool link_up(RTL8169State *s)
{
    return !qemu_get_queue(s->nic)->link_down;
}

static bool config_unlocked(RTL8169State *s)
{
    return (s->regs[CFG9346] & 0xC0) == 0xC0;
}

static void update_irq(RTL8169State *s)
{
    PCIDevice *d = PCI_DEVICE(s);
    bool level = (isr(s) & imr(s)) != 0;

    if (msix_enabled(d)) {
        /* Message interrupts are edges: one per transition. */
        if (level && !s->irq_level) {
            msix_notify(d, 0);
        }
    } else if (msi_enabled(d)) {
        if (level && !s->irq_level) {
            msi_notify(d, 0);
        }
    } else {
        pci_set_irq(d, level);
    }
    s->irq_level = level;
}

static void raise_irq(RTL8169State *s, uint16_t bits)
{
    if (s->is_8125) {
        stl_le_p(&s->regs[ISR8125], reg32(s, ISR8125) | bits);
    } else {
        stw_le_p(&s->regs[ISR], reg16(s, ISR) | bits);
    }
    update_irq(s);
}

/* ---- PHY (MII registers behind PHYAR) ---- */

static void phy_reset(RTL8169State *s)
{
    memset(s->phy, 0, sizeof(s->phy));
    s->phy[0] = 0x1140;     /* BMCR: autonegotiation, full duplex */
    s->phy[2] = 0x001C;     /* PHY id: Realtek */
    s->phy[3] = 0xC912;
    s->phy[4] = 0x01E1;     /* advertise 10/100 */
    s->phy[9] = 0x0300;     /* advertise 1000 */
}

static uint16_t phy_read(RTL8169State *s, int reg)
{
    bool up = link_up(s);

    switch (reg) {
    case 1: /* BMSR */
        return 0x7969 | (up ? 0x0024 : 0);
    case 5: /* link partner */
        return up ? 0xC5E1 : 0;
    case 10: /* 1000BASE-T status */
        return up ? 0x3C00 : 0;
    default:
        return s->phy[reg];
    }
}

static void phy_access(RTL8169State *s)
{
    uint32_t v = reg32(s, PHYAR);
    int reg = (v >> 16) & 0x1F;

    if (v & 0x80000000) {
        uint16_t data = v & 0xFFFF;
        if (reg == 0) {
            data &= ~0x8200;    /* reset and restart autonegotiation self-clear */
        }
        if (reg != 1 && reg != 2 && reg != 3 && reg != 5 && reg != 10) {
            s->phy[reg] = data;
        }
        stl_le_p(&s->regs[PHYAR], v & ~0x80000000);  /* done */
    } else {
        stl_le_p(&s->regs[PHYAR], 0x80000000 | reg << 16 | phy_read(s, reg));
    }
}

/* ---- Reset ---- */

static void soft_reset(RTL8169State *s)
{
    /* What the RST bit does: both units stop, rings start over. */
    s->regs[CR] &= ~(CR_RE | CR_TE | CR_RST);
    if (s->is_8125) {
        stl_le_p(&s->regs[ISR8125], 0);
        stl_le_p(&s->regs[IMR8125], 0);
    } else {
        stw_le_p(&s->regs[ISR], 0);
        stw_le_p(&s->regs[IMR], 0);
    }
    s->rx_index = 0;
    s->tx_index = 0;
    update_irq(s);
}

static void rtl8169_reset(DeviceState *dev)
{
    RTL8169State *s = RTL8169_BASE(dev);

    memset(s->regs, 0, sizeof(s->regs));
    memcpy(&s->regs[IDR0], s->conf.macaddr.a, 6);
    /* Hardware version in TxConfig: RTL8169s, RTL8168E-VL or RTL8125B. */
    stl_le_p(&s->regs[TCR], s->is_8125 ? 0x64100000 : s->is_8168 ? 0x2C800000 : 0x00800000);
    s->regs[CONFIG0] = 0x00;
    phy_reset(s);
    s->rx_index = 0;
    s->tx_index = 0;
    s->irq_level = false;
    if (msix_present(PCI_DEVICE(s))) {
        msix_reset(PCI_DEVICE(s));
    }
    pci_set_irq(PCI_DEVICE(s), 0);
}

/* ---- Transmit ---- */

static void transmit(RTL8169State *s)
{
    PCIDevice *d = PCI_DEVICE(s);
    uint64_t base = reg64(s, TNPDS);
    size_t len = 0;
    bool in_frame = false;
    bool sent = false;
    int walked;

    if (!(s->regs[CR] & CR_TE)) {
        GERR("TxPoll while the transmitter is off: ignored\n");
        return;
    }
    if (base & 0xFF) {
        GERR("transmit ring at %#" PRIx64 " is not 256-byte aligned\n", base);
        raise_irq(s, INT_TER);
        return;
    }
    for (walked = 0; walked < RING_MAX; walked++) {
        uint64_t at = base + (uint64_t)s->tx_index * 16;
        uint32_t opts1;
        uint64_t addr;
        uint32_t size;

        pci_dma_read(d, at, &opts1, 4);
        opts1 = le32_to_cpu(opts1);
        if (!(opts1 & D_OWN)) {
            break;
        }
        pci_dma_read(d, at + 8, &addr, 8);
        addr = le64_to_cpu(addr);
        size = opts1 & 0xFFFF;

        if (opts1 & D_FS) {
            if (in_frame) {
                GERR("FS in the middle of a frame (descriptor %u): frame dropped\n",
                     s->tx_index);
            }
            in_frame = true;
            len = 0;
        } else if (!in_frame) {
            GERR("descriptor %u continues a frame that never started (no FS)\n",
                 s->tx_index);
        }
        if (in_frame) {
            if (size == 0) {
                GERR("descriptor %u has length 0\n", s->tx_index);
            }
            if (len + size > MAX_FRAME) {
                GERR("frame longer than %d bytes: dropped\n", MAX_FRAME);
                in_frame = false;
            } else {
                pci_dma_read(d, addr, s->frame + len, size);
                len += size;
            }
        }
        if (in_frame && (opts1 & D_LS)) {
            if (len < 60) {
                /* Several revisions send runts as they are. */
                GERR("frame of %zu bytes not padded to 60 by the driver\n", len);
                memset(s->frame + len, 0, 60 - len);
                len = 60;
            }
            if (link_up(s)) {
                qemu_send_packet(qemu_get_queue(s->nic), s->frame, len);
            }
            in_frame = false;
            sent = true;
        }
        /* Done: the chip hands the descriptor back. */
        opts1 = cpu_to_le32(opts1 & ~D_OWN);
        pci_dma_write(d, at, &opts1, 4);

        if (le32_to_cpu(opts1) & D_EOR) {
            s->tx_index = 0;
        } else if (++s->tx_index == RING_MAX) {
            GERR("transmit ring of %d descriptors without EOR\n", RING_MAX);
            s->tx_index = 0;
        }
    }
    if (in_frame) {
        GERR("frame without LS: the chip waits for the rest\n");
    }
    raise_irq(s, sent ? INT_TOK : INT_TDU);
}

/* ---- Receive ---- */

static bool rtl8169_can_receive(NetClientState *nc)
{
    RTL8169State *s = qemu_get_nic_opaque(nc);

    return (s->regs[CR] & CR_RE) != 0;
}

static uint32_t accepts(RTL8169State *s, const uint8_t *buf)
{
    uint32_t rcr = reg32(s, RCR);

    if (rcr & RCR_AAP) {
        return memcmp(buf, &s->regs[IDR0], 6) == 0 ? RX_PAM : D_OWN;
    }
    if (is_broadcast_ether_addr(buf)) {
        return (rcr & RCR_AB) ? RX_BAR : 0;
    }
    if (is_multicast_ether_addr(buf)) {
        if (rcr & RCR_AM) {
            int bit = net_crc32(buf, ETH_ALEN) >> 26;
            if (s->regs[MAR0 + (bit >> 3)] & (1 << (bit & 7))) {
                return RX_MAR;
            }
        }
        return 0;
    }
    if ((rcr & RCR_APM) && memcmp(buf, &s->regs[IDR0], 6) == 0) {
        return RX_PAM;
    }
    return 0;
}

static ssize_t rtl8169_receive(NetClientState *nc, const uint8_t *buf, size_t size)
{
    RTL8169State *s = qemu_get_nic_opaque(nc);
    PCIDevice *d = PCI_DEVICE(s);
    uint64_t base = reg64(s, RDSAR);
    uint16_t rms = reg16(s, RMS) & 0x3FFF;
    uint32_t kind;
    size_t total, done;
    uint32_t crc;
    int walked;

    if (!(s->regs[CR] & CR_RE) || size < 14) {
        return size;
    }
    kind = accepts(s, buf);
    if (!kind) {
        return size;
    }
    if (kind == D_OWN) {
        kind = 0;   /* promiscuous, not addressed to us */
    }
    /* On the wire: padded to 60 bytes, then the 4-byte FCS. */
    total = MAX(size, 60);
    if (total + 4 > MAX_FRAME) {
        return size;
    }
    memcpy(s->frame, buf, size);
    memset(s->frame + size, 0, total - size);
    crc = cpu_to_le32(crc32(0, s->frame, total));
    memcpy(s->frame + total, &crc, 4);
    total += 4;

    if (rms == 0) {
        GERR("RxMaxSize is 0: every frame is too long and dropped\n");
        return size;
    }
    if (total > rms) {
        GERR("frame of %zu bytes over RxMaxSize %u: dropped\n", total, rms);
        raise_irq(s, INT_RER);
        return size;
    }
    if (base & 0xFF) {
        GERR("receive ring at %#" PRIx64 " is not 256-byte aligned\n", base);
        raise_irq(s, INT_RER);
        return size;
    }

    done = 0;
    for (walked = 0; walked < RING_MAX && done < total; walked++) {
        uint64_t at = base + (uint64_t)s->rx_index * 16;
        uint32_t opts1, status, zero = 0;
        uint64_t addr;
        size_t room, chunk;

        pci_dma_read(d, at, &opts1, 4);
        opts1 = le32_to_cpu(opts1);
        if (!(opts1 & D_OWN)) {
            /* No buffer: the frame is lost. */
            stl_le_p(&s->regs[MPC], reg32(s, MPC) + 1);
            raise_irq(s, INT_RDU | (done ? INT_RER : 0));
            return size;
        }
        pci_dma_read(d, at + 8, &addr, 8);
        addr = le64_to_cpu(addr);
        room = opts1 & 0x3FFF;
        if (room == 0) {
            GERR("receive descriptor %u has a buffer size of 0\n", s->rx_index);
            raise_irq(s, INT_RER);
            return size;
        }
        if (addr & 7) {
            GERR("receive buffer %#" PRIx64 " is not 8-byte aligned\n", addr);
        }
        chunk = MIN(room, total - done);
        pci_dma_write(d, addr, s->frame + done, chunk);

        status = (opts1 & D_EOR) | kind;
        if (done == 0) {
            status |= D_FS;
        }
        done += chunk;
        if (done == total) {
            status |= D_LS | total;
        } else {
            status |= chunk;
        }
        status = cpu_to_le32(status);
        pci_dma_write(d, at + 4, &zero, 4);
        pci_dma_write(d, at, &status, 4);

        if (opts1 & D_EOR) {
            s->rx_index = 0;
        } else if (++s->rx_index == RING_MAX) {
            GERR("receive ring of %d descriptors without EOR\n", RING_MAX);
            s->rx_index = 0;
        }
    }
    raise_irq(s, INT_ROK);
    return size;
}

static void rtl8169_link_changed(NetClientState *nc)
{
    RTL8169State *s = qemu_get_nic_opaque(nc);

    raise_irq(s, INT_LINKCHG);
}

/* ---- Register access ---- */

static uint8_t read_byte(RTL8169State *s, int off)
{
    if (off == PHYSTATUS) {
        /* full duplex, link, 1000 Mbit/s */
        return link_up(s) ? 0x13 : 0;
    }
    return s->regs[off];
}

static void write_byte(RTL8169State *s, int off, uint8_t v)
{
    bool running = (s->regs[CR] & (CR_RE | CR_TE)) == (CR_RE | CR_TE);

    switch (off) {
    case IDR0 ... IDR0 + 5:
        if (!config_unlocked(s)) {
            GERR("MAC address written while Cfg9346 is locked: ignored\n");
            return;
        }
        break;
    case CONFIG0 ... CONFIG5:
        if (!config_unlocked(s)) {
            GERR("Config%d written while Cfg9346 is locked: ignored\n", off - CONFIG0);
            return;
        }
        break;
    case CR:
        v &= CR_RST | CR_RE | CR_TE;
        break;
    case ISR:
    case ISR + 1:
        if (s->is_8125) {
            /* The upper half of the 8125's interrupt status. */
            s->regs[off] &= ~v;
            return;
        }
        s->regs[off] &= ~v;     /* write 1 to clear */
        return;
    case ISR8125:
    case ISR8125 + 1:
        if (s->is_8125) {
            s->regs[off] &= ~v;
            return;
        }
        break;
    case TCR ... TCR + 3:
        if (!s->is_8168 && !running) {
            return;             /* early RTL8169: only with both units on */
        }
        {
            static const uint8_t writable[4] = { 0x00, 0x07, 0x07, 0x03 };
            int i = off - TCR;
            v = (s->regs[off] & ~writable[i]) | (v & writable[i]);
        }
        break;
    case RCR ... RCR + 3:
        if (!s->is_8168 && !running) {
            return;
        }
        break;
    case PHYSTATUS:
    case MPC + 1 ... MPC + 3:
        return;                 /* read only */
    case MPC:
        stl_le_p(&s->regs[MPC], 0);     /* any write clears it */
        return;
    }
    s->regs[off] = v;
}

static bool touches(hwaddr addr, unsigned size, int off)
{
    return addr <= off && off < addr + size;
}

static uint64_t rtl8169_read(void *opaque, hwaddr addr, unsigned size)
{
    RTL8169State *s = opaque;
    uint64_t v = 0;
    unsigned i;

    if (addr + size > 256) {
        return 0;
    }
    for (i = 0; i < size; i++) {
        v |= (uint64_t)read_byte(s, addr + i) << (8 * i);
    }
    return v;
}

static void rtl8169_write(void *opaque, hwaddr addr, uint64_t val, unsigned size)
{
    RTL8169State *s = opaque;
    bool rx_was_on = s->regs[CR] & CR_RE;
    unsigned i;

    if (addr + size > 256) {
        return;
    }
    if ((s->regs[CR] & CR_RE) && (touches(addr, size, RDSAR) || touches(addr, size, RDSAR + 4))) {
        GERR("receive ring address changed while the receiver is on\n");
    }
    if ((s->regs[CR] & CR_TE) && (touches(addr, size, TNPDS) || touches(addr, size, TNPDS + 4))) {
        GERR("transmit ring address changed while the transmitter is on\n");
    }
    for (i = 0; i < size; i++) {
        write_byte(s, addr + i, val >> (8 * i));
    }

    if (touches(addr, size, CR)) {
        if (s->regs[CR] & CR_RST) {
            soft_reset(s);
        } else if (!rx_was_on && (s->regs[CR] & CR_RE)) {
            qemu_flush_queued_packets(qemu_get_queue(s->nic));
        }
    }
    if (touches(addr, size, RDSAR)) {
        s->rx_index = 0;
    }
    if (touches(addr, size, TNPDS)) {
        s->tx_index = 0;
    }
    if (s->is_8125) {
        if (touches(addr, size, TPPOLL8125)) {
            uint8_t poll = s->regs[TPPOLL8125];
            s->regs[TPPOLL8125] = 0;
            if (poll & 1) {
                transmit(s);
            }
        }
    } else if (touches(addr, size, TPPOLL)) {
        uint8_t poll = s->regs[TPPOLL];
        s->regs[TPPOLL] = 0;
        if (poll & POLL_HPQ) {
            GERR("high priority queue polled: not modelled\n");
        }
        if (poll & POLL_NPQ) {
            transmit(s);
        }
    }
    if (touches(addr, size, PHYAR + 3)) {
        phy_access(s);
    }
    if (touches(addr, size, ISR) || touches(addr, size, ISR + 1) ||
        touches(addr, size, IMR) || touches(addr, size, IMR + 1) ||
        (s->is_8125 && (touches(addr, size, IMR8125) || touches(addr, size, IMR8125 + 3)))) {
        update_irq(s);
    }
}

static const MemoryRegionOps rtl8169_ops = {
    .read = rtl8169_read,
    .write = rtl8169_write,
    .endianness = DEVICE_LITTLE_ENDIAN,
    .impl = { .min_access_size = 1, .max_access_size = 4 },
    .valid = { .min_access_size = 1, .max_access_size = 4 },
};

static NetClientInfo net_rtl8169_info = {
    .type = NET_CLIENT_DRIVER_NIC,
    .size = sizeof(NICState),
    .can_receive = rtl8169_can_receive,
    .receive = rtl8169_receive,
    .link_status_changed = rtl8169_link_changed,
};

static void rtl8169_realize(PCIDevice *dev, Error **errp)
{
    RTL8169State *s = RTL8169_BASE(dev);
    DeviceState *d = DEVICE(dev);

    s->is_8125 = pci_get_word(dev->config + PCI_DEVICE_ID) == 0x8125;
    s->is_8168 = pci_get_word(dev->config + PCI_DEVICE_ID) == 0x8168 || s->is_8125;
    dev->config[PCI_INTERRUPT_PIN] = 1;

    memory_region_init_io(&s->io, OBJECT(s), &rtl8169_ops, s, "rtl8169-io", 0x100);
    pci_register_bar(dev, 0, PCI_BASE_ADDRESS_SPACE_IO, &s->io);
    if (s->is_8168) {
        memory_region_init_io(&s->mmio, OBJECT(s), &rtl8169_ops, s,
                              s->is_8125 ? "rtl8125-mmio" : "rtl8168-mmio", 0x1000);
        pci_register_bar(dev, 2, PCI_BASE_ADDRESS_SPACE_MEMORY | PCI_BASE_ADDRESS_MEM_TYPE_64,
                         &s->mmio);
        if (msi_init(dev, 0x50, 1, true, false, errp) < 0) {
            return;
        }
        if (msix_init_exclusive_bar(dev, 1, 4, errp) < 0) {
            return;
        }
        msix_vector_use(dev, 0);
    } else {
        memory_region_init_io(&s->mmio, OBJECT(s), &rtl8169_ops, s, "rtl8169-mmio", 0x100);
        pci_register_bar(dev, 1, PCI_BASE_ADDRESS_SPACE_MEMORY, &s->mmio);
    }

    qemu_macaddr_default_if_unset(&s->conf.macaddr);
    s->nic = qemu_new_nic(&net_rtl8169_info, &s->conf, object_get_typename(OBJECT(dev)),
                          d->id, &d->mem_reentrancy_guard, s);
    qemu_format_nic_info_str(qemu_get_queue(s->nic), s->conf.macaddr.a);
}

static void rtl8169_exit(PCIDevice *dev)
{
    RTL8169State *s = RTL8169_BASE(dev);

    if (s->is_8168) {
        msix_uninit_exclusive_bar(dev);
        msi_uninit(dev);
    }
    qemu_del_nic(s->nic);
}

static void rtl8169_instance_init(Object *obj)
{
    RTL8169State *s = RTL8169_BASE(obj);

    device_add_bootindex_property(obj, &s->conf.bootindex, "bootindex",
                                  "/ethernet-phy@0", DEVICE(obj));
}

static Property rtl8169_properties[] = {
    DEFINE_NIC_PROPERTIES(RTL8169State, conf),
    DEFINE_PROP_END_OF_LIST(),
};

static void rtl8169_base_class_init(ObjectClass *klass, void *data)
{
    DeviceClass *dc = DEVICE_CLASS(klass);
    PCIDeviceClass *k = PCI_DEVICE_CLASS(klass);

    k->realize = rtl8169_realize;
    k->exit = rtl8169_exit;
    k->vendor_id = 0x10EC;
    k->class_id = PCI_CLASS_NETWORK_ETHERNET;
    dc->reset = rtl8169_reset;
    device_class_set_props(dc, rtl8169_properties);
    set_bit(DEVICE_CATEGORY_NETWORK, dc->categories);
}

static void rtl8169_class_init(ObjectClass *klass, void *data)
{
    PCIDeviceClass *k = PCI_DEVICE_CLASS(klass);

    k->device_id = 0x8169;
    k->revision = 0x10;
    DEVICE_CLASS(klass)->desc = "Realtek RTL8169 Gigabit Ethernet";
}

static void rtl8168_class_init(ObjectClass *klass, void *data)
{
    PCIDeviceClass *k = PCI_DEVICE_CLASS(klass);

    k->device_id = 0x8168;
    k->revision = 0x06;
    DEVICE_CLASS(klass)->desc = "Realtek RTL8111/8168 Gigabit Ethernet";
}

static void rtl8125_class_init(ObjectClass *klass, void *data)
{
    PCIDeviceClass *k = PCI_DEVICE_CLASS(klass);

    k->device_id = 0x8125;
    k->revision = 0x05;
    DEVICE_CLASS(klass)->desc = "Realtek RTL8125 2.5 Gigabit Ethernet";
}

static const TypeInfo rtl8169_types[] = {
    {
        .name = TYPE_RTL8169_BASE,
        .parent = TYPE_PCI_DEVICE,
        .instance_size = sizeof(RTL8169State),
        .class_init = rtl8169_base_class_init,
        .instance_init = rtl8169_instance_init,
        .abstract = true,
        .interfaces = (InterfaceInfo[]) {
            { INTERFACE_CONVENTIONAL_PCI_DEVICE },
            { },
        },
    },
    {
        .name = "rtl8169",
        .parent = TYPE_RTL8169_BASE,
        .class_init = rtl8169_class_init,
    },
    {
        .name = "rtl8168",
        .parent = TYPE_RTL8169_BASE,
        .class_init = rtl8168_class_init,
    },
    {
        .name = "rtl8125",
        .parent = TYPE_RTL8169_BASE,
        .class_init = rtl8125_class_init,
    },
};

DEFINE_TYPES(rtl8169_types)
