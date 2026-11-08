/* /dev/ashmem as Android's libcutils uses it: a region made with a name
 * and a size, mapped, handed to another process (by fork here, over binder
 * or a unix socket on Android) which sees the same memory; the prot mask
 * that only takes rights away; the device numbers libcutils compares.
 * Prints "am:" lines for scripts/test-linux.sh. */
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>

#define ASHMEM_NAME_LEN 256
#define __ASHMEMIOC 0x77
struct ashmem_pin {
    uint32_t offset, len;
};
#define ASHMEM_SET_NAME _IOW(__ASHMEMIOC, 1, char[ASHMEM_NAME_LEN])
#define ASHMEM_GET_NAME _IOR(__ASHMEMIOC, 2, char[ASHMEM_NAME_LEN])
#define ASHMEM_SET_SIZE _IOW(__ASHMEMIOC, 3, size_t)
#define ASHMEM_GET_SIZE _IO(__ASHMEMIOC, 4)
#define ASHMEM_SET_PROT_MASK _IOW(__ASHMEMIOC, 5, unsigned long)
#define ASHMEM_GET_PROT_MASK _IO(__ASHMEMIOC, 6)
#define ASHMEM_PIN _IOW(__ASHMEMIOC, 7, struct ashmem_pin)
#define ASHMEM_UNPIN _IOW(__ASHMEMIOC, 8, struct ashmem_pin)
#define ASHMEM_GET_PIN_STATUS _IO(__ASHMEMIOC, 9)

int main(void) {
    int fd = open("/dev/ashmem", O_RDWR | O_CLOEXEC);
    if (fd < 0) {
        printf("am: open failed errno=%d\n", errno);
        return 1;
    }
    struct stat dev, st;
    stat("/dev/ashmem", &dev);
    fstat(fd, &st);
    printf("am: open chr=%d same=%d\n", S_ISCHR(st.st_mode) && st.st_rdev != 0, st.st_rdev == dev.st_rdev);

    /* Nothing to map before the size. */
    void *none = mmap(NULL, 4096, PROT_READ, MAP_SHARED, fd, 0);
    int early = none == MAP_FAILED ? errno : 0;

    char name[ASHMEM_NAME_LEN] = "justos-region";
    int e = ioctl(fd, ASHMEM_SET_NAME, name);
    e |= ioctl(fd, ASHMEM_SET_SIZE, (size_t)10000);
    int size = ioctl(fd, ASHMEM_GET_SIZE, NULL);
    memset(name, 0, sizeof name);
    e |= ioctl(fd, ASHMEM_GET_NAME, name);
    printf("am: region name=%s size=%d early=%d e=%d\n", name, size, early == EINVAL, e);

    char *p = mmap(NULL, 10000, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    if (p == MAP_FAILED) {
        printf("am: mmap failed errno=%d\n", errno);
        return 1;
    }
    strcpy(p, "from the parent");
    p[9999] = 'z';
    /* Too much, or a new size once mapped. */
    void *big = mmap(NULL, 4 * 4096, PROT_READ, MAP_SHARED, fd, 0);
    int toobig = big == MAP_FAILED && errno == EINVAL;
    int resize = ioctl(fd, ASHMEM_SET_SIZE, (size_t)20000) < 0 && errno == EINVAL;
    struct ashmem_pin pin = {0, 0};
    int unpinned = ioctl(fd, ASHMEM_UNPIN, &pin);
    int pinned = ioctl(fd, ASHMEM_PIN, &pin);
    int status = ioctl(fd, ASHMEM_GET_PIN_STATUS, &pin);
    printf("am: map got=%s toobig=%d resize=%d pin=%d/%d/%d\n", p, toobig, resize, unpinned, pinned, status);

    /* A private mapping is zeroes of its own. */
    char *priv = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_PRIVATE, fd, 0);
    printf("am: private zero=%d\n", priv != MAP_FAILED && priv[0] == 0);

    pid_t pid = fork();
    if (pid == 0) {
        /* The child maps the region again from the descriptor. */
        char *q = mmap(NULL, 10000, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
        if (q == MAP_FAILED)
            _exit(2);
        int ok = strcmp(q, "from the parent") == 0 && q[9999] == 'z';
        strcpy(q + 4096, "from the child");
        _exit(ok ? 0 : 3);
    }
    int ws = 0;
    waitpid(pid, &ws, 0);
    printf("am: shared child=%d back=%s\n", WEXITSTATUS(ws), p + 4096);

    /* Rights only go: read-only, then writing maps fail. */
    e = ioctl(fd, ASHMEM_SET_PROT_MASK, (unsigned long)PROT_READ);
    int mask = ioctl(fd, ASHMEM_GET_PROT_MASK, NULL);
    int back = ioctl(fd, ASHMEM_SET_PROT_MASK, (unsigned long)(PROT_READ | PROT_WRITE)) < 0 && errno == EINVAL;
    void *w = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    int denied = w == MAP_FAILED && errno == EPERM;
    char *r = mmap(NULL, 4096, PROT_READ, MAP_SHARED, fd, 0);
    printf("am: mask=%d back=%d denied=%d ro=%s e=%d\n", mask, back, denied, r == MAP_FAILED ? "-" : r, e);

    /* The boot-id name Android 10 and later try first. */
    int fd2 = open("/dev/ashmem00000000-0000-0000-0000-000000000000", O_RDWR | O_CLOEXEC);
    struct stat st2;
    fstat(fd2, &st2);
    printf("am: done bootid=%d\n", fd2 >= 0 && st2.st_rdev == dev.st_rdev);
    return 0;
}
