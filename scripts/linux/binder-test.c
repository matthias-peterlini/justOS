/* Checks the binder driver (/dev/binder) the way Android's libbinder uses
 * it, with two processes: the parent is the context manager, the child a
 * client calling it through handle 0. Two-way calls, an object of the
 * client sent as a handle (and the counts its owner hears of), a call back
 * into the waiting client, a descriptor, one-way calls in order, buffers
 * with pointers (scatter-gather), the client's object coming back home,
 * and the death notice when the client ends.
 *
 * Build: gcc -O2 -o bt binder-test.c */
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include <unistd.h>

#define BINDER_WRITE_READ 0xc0306201UL
#define BINDER_SET_MAX_THREADS 0x40046205UL
#define BINDER_SET_CONTEXT_MGR 0x40046207UL
#define BINDER_VERSION 0xc0046209UL

#define BC_TRANSACTION 0x40406300u
#define BC_REPLY 0x40406301u
#define BC_FREE_BUFFER 0x40086303u
#define BC_INCREFS 0x40046304u
#define BC_ACQUIRE 0x40046305u
#define BC_RELEASE 0x40046306u
#define BC_DECREFS 0x40046307u
#define BC_INCREFS_DONE 0x40106308u
#define BC_ACQUIRE_DONE 0x40106309u
#define BC_ENTER_LOOPER 0x630cu
#define BC_REQUEST_DEATH_NOTIFICATION 0x400c630eu
#define BC_CLEAR_DEATH_NOTIFICATION 0x400c630fu
#define BC_DEAD_BINDER_DONE 0x40086310u
#define BC_TRANSACTION_SG 0x40486311u

#define BR_ERROR 0x80047200u
#define BR_TRANSACTION_SEC_CTX 0x80487202u
#define BR_TRANSACTION 0x80407202u
#define BR_REPLY 0x80407203u
#define BR_DEAD_REPLY 0x7205u
#define BR_TRANSACTION_COMPLETE 0x7206u
#define BR_INCREFS 0x80107207u
#define BR_ACQUIRE 0x80107208u
#define BR_RELEASE 0x80107209u
#define BR_DECREFS 0x8010720au
#define BR_NOOP 0x720cu
#define BR_SPAWN_LOOPER 0x720du
#define BR_DEAD_BINDER 0x8008720fu
#define BR_CLEAR_DEATH_NOTIFICATION_DONE 0x80087210u
#define BR_FAILED_REPLY 0x7211u

#define TYPE_BINDER 0x73622a85u
#define TYPE_HANDLE 0x73682a85u
#define TYPE_FD 0x66642a85u
#define TYPE_PTR 0x70742a85u
#define TF_ONE_WAY 1u
#define TF_ACCEPT_FDS 0x10u
#define HAS_PARENT 1u

#define MAP_SIZE (1024 * 1024 - 8192)

struct bwr {
    uint64_t write_size, write_consumed, write_buffer;
    uint64_t read_size, read_consumed, read_buffer;
};
struct txn {
    uint64_t target, cookie;
    uint32_t code, flags;
    int32_t sender_pid;
    uint32_t sender_euid;
    uint64_t data_size, offsets_size, buffer, offsets;
};
struct fbo {
    uint32_t type, flags;
    uint64_t binder, cookie;
};
struct fdo {
    uint32_t type, pad;
    uint32_t fd, pad2;
    uint64_t cookie;
};
struct bufobj {
    uint32_t type, flags;
    uint64_t buffer, length, parent, parent_offset;
};

/* A parcel: data and the offsets of the objects in it. */
typedef struct {
    uint8_t data[512];
    size_t len;
    uint64_t offs[8];
    int n;
} P;

typedef struct B B;
typedef void (*handler_fn)(B *b, struct txn *t, P *reply);
struct B {
    int fd;
    uint8_t *map;
    handler_fn handler;
    int poll;
};

static int g_increfs, g_acquire, g_release, g_decrefs, g_spawn, g_clear, g_dead;
static uint64_t g_dead_cookie;
static pid_t child_pid;
static uint32_t client_handle;
static int sum, order;

static void p_str(P *p, const char *s) {
    size_t n = strlen(s) + 1;
    memcpy(p->data + p->len, s, n);
    p->len += (n + 7) & ~7u;
}
static void p_u32(P *p, uint32_t v) {
    memcpy(p->data + p->len, &v, 4);
    p->len += 8;
}
static void p_obj(P *p, const void *o, size_t n) {
    p->len = (p->len + 7) & ~7u;
    p->offs[p->n++] = p->len;
    memcpy(p->data + p->len, o, n);
    p->len += n;
}

static void wr(B *b, const void *data, size_t len) {
    struct bwr w = {0};
    w.write_size = len;
    w.write_buffer = (uintptr_t)data;
    if (ioctl(b->fd, BINDER_WRITE_READ, &w) < 0 || w.write_consumed != len) {
        printf("bt: write failed errno=%d consumed=%lu\n", errno, (unsigned long)w.write_consumed);
        exit(7);
    }
}

static void cmd32(B *b, uint32_t cmd, uint32_t v) {
    struct __attribute__((packed)) { uint32_t c, v; } d = {cmd, v};
    wr(b, &d, sizeof d);
}

static void cmd64(B *b, uint32_t cmd, uint64_t v) {
    struct __attribute__((packed)) { uint32_t c; uint64_t v; } d = {cmd, v};
    wr(b, &d, sizeof d);
}

static void death(B *b, uint32_t cmd, uint32_t handle, uint64_t cookie) {
    struct __attribute__((packed)) { uint32_t c, h; uint64_t k; } d = {cmd, handle, cookie};
    wr(b, &d, sizeof d);
}

static void send_txn(B *b, uint32_t cmd, uint64_t target, uint32_t code, uint32_t flags, P *p, uint64_t sg) {
    struct __attribute__((packed)) {
        uint32_t c;
        struct txn t;
        uint64_t buffers_size;
    } w;
    memset(&w, 0, sizeof w);
    w.c = cmd;
    w.t.target = target;
    w.t.code = code;
    w.t.flags = flags;
    w.t.data_size = p->len;
    w.t.offsets_size = p->n * 8;
    w.t.buffer = (uintptr_t)p->data;
    w.t.offsets = (uintptr_t)p->offs;
    w.buffers_size = sg;
    wr(b, &w, cmd == BC_TRANSACTION_SG ? sizeof w : sizeof w - 8);
}

/* Reads from the driver and handles what comes, until a reply arrives
 * (want_reply), or until the client's death was cleared. */
static int run(B *b, struct txn *rep, uint8_t *rdata, size_t rcap, int want_reply) {
    uint8_t buf[256];
    for (;;) {
        if (!want_reply && g_dead && g_clear)
            return 0;
        if (b->poll) {
            struct pollfd pf = {b->fd, POLLIN, 0};
            if (poll(&pf, 1, 15000) <= 0) {
                printf("bt: poll timeout\n");
                exit(9);
            }
        }
        struct bwr w = {0};
        w.read_size = sizeof buf;
        w.read_buffer = (uintptr_t)buf;
        if (ioctl(b->fd, BINDER_WRITE_READ, &w) < 0) {
            if (errno == EINTR)
                continue;
            printf("bt: read failed errno=%d\n", errno);
            exit(7);
        }
        uint8_t *p = buf, *end = buf + w.read_consumed;
        while (p < end) {
            uint32_t cmd;
            memcpy(&cmd, p, 4);
            p += 4;
            switch (cmd) {
            case BR_NOOP:
            case BR_TRANSACTION_COMPLETE:
                break;
            case BR_SPAWN_LOOPER:
                g_spawn++;
                break;
            case BR_INCREFS:
            case BR_ACQUIRE: {
                struct __attribute__((packed)) { uint32_t c; uint64_t ptr, cookie; } d;
                memcpy(&d.ptr, p, 16);
                p += 16;
                if (cmd == BR_INCREFS)
                    g_increfs++;
                else
                    g_acquire++;
                d.c = cmd == BR_INCREFS ? BC_INCREFS_DONE : BC_ACQUIRE_DONE;
                wr(b, &d, sizeof d);
                break;
            }
            case BR_RELEASE:
                g_release++;
                p += 16;
                break;
            case BR_DECREFS:
                g_decrefs++;
                p += 16;
                break;
            case BR_TRANSACTION:
            case BR_TRANSACTION_SEC_CTX: {
                struct txn t;
                memcpy(&t, p, sizeof t);
                p += sizeof t + (cmd == BR_TRANSACTION_SEC_CTX ? 8 : 0);
                P r = {0};
                b->handler(b, &t, &r);
                cmd64(b, BC_FREE_BUFFER, t.buffer);
                if (!(t.flags & TF_ONE_WAY))
                    send_txn(b, BC_REPLY, 0, 0, 0, &r, 0);
                break;
            }
            case BR_REPLY: {
                struct txn t;
                memcpy(&t, p, sizeof t);
                p += sizeof t;
                if (rep) {
                    *rep = t;
                    memset(rdata, 0, rcap);
                    memcpy(rdata, (void *)(uintptr_t)t.buffer, t.data_size < rcap ? t.data_size : rcap);
                }
                cmd64(b, BC_FREE_BUFFER, t.buffer);
                return 0;
            }
            case BR_DEAD_REPLY:
                return -1;
            case BR_FAILED_REPLY:
                return -2;
            case BR_DEAD_BINDER: {
                uint64_t c;
                memcpy(&c, p, 8);
                p += 8;
                g_dead++;
                g_dead_cookie = c;
                cmd64(b, BC_DEAD_BINDER_DONE, c);
                /* as libbinder's proxies do once they heard */
                death(b, BC_CLEAR_DEATH_NOTIFICATION, client_handle, c);
                break;
            }
            case BR_CLEAR_DEATH_NOTIFICATION_DONE:
                p += 8;
                g_clear++;
                break;
            case BR_ERROR:
                p += 4;
                break;
            default:
                printf("bt: unknown return %#x\n", cmd);
                exit(8);
            }
        }
    }
}

static int call(B *b, uint32_t handle, uint32_t code, P *p, uint8_t *rdata, size_t rcap, uint32_t cmd, uint64_t sg) {
    struct txn rt;
    send_txn(b, cmd, handle, code, TF_ACCEPT_FDS, p, sg);
    return run(b, &rt, rdata, rcap, 1);
}

static B bopen(void) {
    B b = {0};
    b.fd = open("/dev/binder", O_RDWR | O_CLOEXEC);
    if (b.fd < 0) {
        printf("bt: open failed errno=%d\n", errno);
        exit(2);
    }
    b.map = mmap(NULL, MAP_SIZE, PROT_READ, MAP_PRIVATE | MAP_NORESERVE, b.fd, 0);
    if (b.map == MAP_FAILED) {
        printf("bt: mmap failed errno=%d\n", errno);
        exit(2);
    }
    return b;
}

/* ------------------------------------------------------------------------ */
/* The client */

static void client_handler(B *b, struct txn *t, P *r) {
    (void)b;
    char s[64];
    snprintf(s, sizeof s, "%s", t->target == 0x1234 && t->cookie == 0x5678 && t->code == 30 ? "nested-ok" : "nested-wrong");
    p_str(r, s);
}

static void client(void) {
    B c = bopen();
    c.handler = client_handler;
    uint8_t rd[128];
    int busy = ioctl(c.fd, BINDER_SET_CONTEXT_MGR, 0) < 0 && errno == EBUSY;

    P p = {0};
    p_str(&p, "ping");
    int e = call(&c, 0, 1, &p, rd, sizeof rd, BC_TRANSACTION, 0);
    printf("bt: ping %s e=%d busy=%d\n", rd, e, busy);

    /* An object of ours, which the manager holds by a handle. */
    P q = {0};
    struct fbo o = {TYPE_BINDER, 0x7f | 0x100, 0x1234, 0x5678};
    p_obj(&q, &o, sizeof o);
    e = call(&c, 0, 2, &q, rd, sizeof rd, BC_TRANSACTION, 0);
    printf("bt: register %s increfs=%d acquire=%d e=%d\n", rd, g_increfs, g_acquire, e);

    /* The manager calls our object while we wait for it. */
    P n = {0};
    e = call(&c, 0, 3, &n, rd, sizeof rd, BC_TRANSACTION, 0);
    printf("bt: nested reply=%s e=%d\n", rd, e);

    /* A descriptor. */
    int pp[2];
    if (pipe(pp) == 0) {
        if (write(pp[1], "via binder fd", 13) != 13)
            printf("bt: pipe write failed\n");
        close(pp[1]);
    }
    P f = {0};
    struct fdo fo = {TYPE_FD, 0, (uint32_t)pp[0], 0, 0};
    p_obj(&f, &fo, sizeof fo);
    e = call(&c, 0, 4, &f, rd, sizeof rd, BC_TRANSACTION, 0);
    close(pp[0]);
    printf("bt: fd %s e=%d\n", rd, e);

    /* One-way calls to an object arrive one after the other, in order. */
    for (uint32_t v = 1; v <= 3; v++) {
        P w = {0};
        p_u32(&w, v);
        send_txn(&c, BC_TRANSACTION, 0, 5, TF_ONE_WAY, &w, 0);
    }
    /* (a two-way call may pass the ones still queued, as on Linux) */
    for (int k = 0; k < 50; k++) {
        P s = {0};
        e = call(&c, 0, 6, &s, rd, sizeof rd, BC_TRANSACTION, 0);
        if (strstr((char *)rd, "sum=6"))
            break;
        usleep(20000);
    }
    printf("bt: oneway %s e=%d\n", rd, e);

    /* A buffer with a pointer into another (scatter-gather). */
    static const char name[] = "scatter-gather";
    uint64_t outer[2] = {(uintptr_t)name, sizeof name};
    P g = {0};
    struct bufobj a = {TYPE_PTR, 0, (uintptr_t)outer, sizeof outer, 0, 0};
    struct bufobj nb = {TYPE_PTR, HAS_PARENT, (uintptr_t)name, sizeof name, 0, 0};
    p_obj(&g, &a, sizeof a);
    p_obj(&g, &nb, sizeof nb);
    e = call(&c, 0, 7, &g, rd, sizeof rd, BC_TRANSACTION_SG, sizeof outer + ((sizeof name + 7) & ~7u));
    printf("bt: sg %s e=%d\n", rd, e);

    /* Our object sent back to us is ours again, not a handle. */
    P h = {0};
    e = call(&c, 0, 8, &h, rd, sizeof rd, BC_TRANSACTION, 0);
    struct fbo back;
    memcpy(&back, rd, sizeof back);
    printf("bt: home type=%s ptr=%#lx cookie=%#lx e=%d\n", back.type == TYPE_BINDER ? "binder" : "other", (unsigned long)back.binder, (unsigned long)back.cookie, e);
    exit(0);
}

/* ------------------------------------------------------------------------ */
/* The context manager */

static void manager_handler(B *b, struct txn *t, P *r) {
    const uint8_t *d = (const uint8_t *)(uintptr_t)t->buffer;
    const uint64_t *offs = (const uint64_t *)(uintptr_t)t->offsets;
    char s[96];
    switch (t->code) {
    case 1:
        snprintf(s, sizeof s, "%s pid=%d euid=%d", (const char *)d, t->sender_pid == child_pid, t->sender_euid == geteuid());
        p_str(r, s);
        break;
    case 2: {
        const struct fbo *o = (const void *)(d + offs[0]);
        client_handle = (uint32_t)o->binder;
        cmd32(b, BC_INCREFS, client_handle);
        cmd32(b, BC_ACQUIRE, client_handle);
        death(b, BC_REQUEST_DEATH_NOTIFICATION, client_handle, 0xdead);
        snprintf(s, sizeof s, "handle=%u type=%d", client_handle, o->type == TYPE_HANDLE);
        p_str(r, s);
        break;
    }
    case 3: {
        P q = {0};
        uint8_t rd[64];
        p_str(&q, "hello?");
        int e = call(b, client_handle, 30, &q, rd, sizeof rd, BC_TRANSACTION, 0);
        snprintf(s, sizeof s, "via-%s/%d", (const char *)rd, e);
        p_str(r, s);
        break;
    }
    case 4: {
        const struct fdo *o = (const void *)(d + offs[0]);
        char got[32] = {0};
        int fd = (int)o->fd;
        ssize_t n = read(fd, got, sizeof got - 1);
        int cloexec = (fcntl(fd, F_GETFD) & FD_CLOEXEC) != 0;
        close(fd);
        snprintf(s, sizeof s, "got=%s n=%d new=%d cloexec=%d", got, (int)n, fd > 2, cloexec);
        p_str(r, s);
        break;
    }
    case 5: {
        uint32_t v;
        memcpy(&v, d, 4);
        sum += v;
        order = order * 10 + v;
        break;
    }
    case 6:
        snprintf(s, sizeof s, "sum=%d order=%d", sum, order);
        p_str(r, s);
        break;
    case 7: {
        const struct bufobj *a = (const void *)(d + offs[0]);
        const uint64_t *outer = (const uint64_t *)(uintptr_t)a->buffer;
        const char *name = (const char *)(uintptr_t)outer[0];
        int inarea = (const uint8_t *)name >= b->map && (const uint8_t *)name < b->map + MAP_SIZE;
        snprintf(s, sizeof s, "got=%s len=%lu inarea=%d", name, (unsigned long)outer[1], inarea);
        p_str(r, s);
        break;
    }
    case 8: {
        struct fbo o = {TYPE_HANDLE, 0, client_handle, 0};
        p_obj(r, &o, sizeof o);
        break;
    }
    default:
        p_str(r, "?");
    }
}

int main(void) {
    setvbuf(stdout, NULL, _IONBF, 0);
    int fd = open("/dev/binder", O_RDWR | O_CLOEXEC);
    if (fd < 0) {
        printf("bt: open failed errno=%d\n", errno);
        return 2;
    }
    int32_t version = 0;
    ioctl(fd, BINDER_VERSION, &version);
    void *w = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    int nowrite = w == MAP_FAILED && errno == EPERM;
    close(fd);
    printf("bt: version %d nowrite=%d\n", version, nowrite);

    B m = bopen();
    m.handler = manager_handler;
    m.poll = 1;
    uint32_t one = 1;
    ioctl(m.fd, BINDER_SET_MAX_THREADS, &one);
    if (ioctl(m.fd, BINDER_SET_CONTEXT_MGR, 0) < 0) {
        printf("bt: context manager failed errno=%d\n", errno);
        return 3;
    }
    child_pid = fork();
    if (child_pid == 0) {
        close(m.fd);
        munmap(m.map, MAP_SIZE);
        client();
    }
    uint32_t enter = BC_ENTER_LOOPER;
    wr(&m, &enter, 4);
    run(&m, NULL, NULL, 0, 0);
    /* (run returns once the client's death came and was cleared) */
    printf("bt: dead cookie=%#lx clear=%d spawn=%d\n", (unsigned long)g_dead_cookie, g_clear, g_spawn);
    P q = {0};
    uint8_t rd[16];
    int e = call(&m, client_handle, 31, &q, rd, sizeof rd, BC_TRANSACTION, 0);
    cmd32(&m, BC_RELEASE, client_handle);
    cmd32(&m, BC_DECREFS, client_handle);
    int status = -1;
    waitpid(child_pid, &status, 0);
    printf("bt: done deadcall=%d exit=%d\n", e, WIFEXITED(status) ? WEXITSTATUS(status) : -1);
    return 0;
}
