/* Shared mappings of a file, as Android's property areas use them: one
 * process maps the file to write, others map it read-only and wait on a
 * futex in it; what one stores, the others see, and read() and write() of
 * the file agree with the mappings; the pages go back to the file.
 * Prints "ms:" lines for scripts/test-linux.sh. */
#include <errno.h>
#include <fcntl.h>
#include <linux/futex.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#define SIZE 8192
#define NAME "ms.dat"

static long futex(uint32_t *addr, int op, uint32_t val, const struct timespec *t) {
    return syscall(SYS_futex, addr, op, val, t, NULL, 0);
}

int main(void) {
    unlink(NAME);
    int fd = open(NAME, O_RDWR | O_CREAT | O_EXCL | O_CLOEXEC, 0644);
    if (fd < 0 || ftruncate(fd, SIZE) != 0) {
        printf("ms: create failed errno=%d\n", errno);
        return 1;
    }
    char *p = mmap(NULL, SIZE, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    if (p == MAP_FAILED) {
        printf("ms: mmap failed errno=%d\n", errno);
        return 1;
    }
    strcpy(p + 100, "first");
    uint32_t *serial = (uint32_t *)(p + 4096);

    pid_t pid = fork();
    if (pid == 0) {
        /* A reader: its own read-only mapping of the file, waiting on the
         * serial as bionic's property readers do. */
        int rfd = open(NAME, O_RDONLY | O_CLOEXEC);
        const char *q = mmap(NULL, SIZE, PROT_READ, MAP_SHARED, rfd, 0);
        if (q == MAP_FAILED)
            _exit(2);
        uint32_t *s = (uint32_t *)(q + 4096);
        int first = strcmp(q + 100, "first") == 0;
        struct timespec t = {5, 0};
        while (__atomic_load_n(s, __ATOMIC_ACQUIRE) == 0)
            if (futex(s, FUTEX_WAIT, 0, &t) != 0 && errno == ETIMEDOUT)
                _exit(3);
        _exit(first && strcmp(q + 100, "second") == 0 ? 0 : 4);
    }
    /* Let the reader get to its wait. */
    usleep(300 * 1000);
    strcpy(p + 100, "second");
    __atomic_store_n(serial, 1, __ATOMIC_RELEASE);
    long woken = futex(serial, FUTEX_WAKE, 1, NULL);
    int ws = 0;
    waitpid(pid, &ws, 0);
    printf("ms: reader exit=%d woken=%ld\n", WEXITSTATUS(ws), woken);

    /* A writer in another process: the parent sees what it stores. */
    pid = fork();
    if (pid == 0) {
        int wfd = open(NAME, O_RDWR | O_CLOEXEC);
        char *q = mmap(NULL, SIZE, PROT_READ | PROT_WRITE, MAP_SHARED, wfd, 0);
        if (q == MAP_FAILED)
            _exit(2);
        strcpy(q + 300, "from the child");
        _exit(0);
    }
    waitpid(pid, &ws, 0);
    printf("ms: child wrote=%s\n", p + 300);

    /* read() sees the mapping, the mapping sees write(). */
    char buf[32] = {0};
    pread(fd, buf, 6, 100);
    pwrite(fd, "by pwrite", 10, 200);
    printf("ms: pread=%s mapped=%s msync=%d\n", buf, p + 200, msync(p, SIZE, MS_SYNC));

    /* Cut and grown again: zeroes. */
    ftruncate(fd, 0);
    ftruncate(fd, SIZE);
    int zero = p[100] == 0 && p[300] == 0;
    strcpy(p + 500, "kept");
    munmap(p, SIZE);
    close(fd);

    /* Unmapped: the pages went back to the file. */
    fd = open(NAME, O_RDONLY | O_CLOEXEC);
    memset(buf, 0, sizeof buf);
    pread(fd, buf, 5, 500);
    close(fd);
    unlink(NAME);
    printf("ms: done zero=%d back=%s\n", zero, buf);
    return 0;
}
