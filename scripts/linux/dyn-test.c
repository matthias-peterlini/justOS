/* A dynamically linked glibc program for the Linux layer: the dynamic
 * loader, shared libraries, threads in libc, dlopen, new processes
 * (fork, vfork, posix_spawn, execve of a #! script, waitpid), signals
 * (handlers, masks, faults, EINTR and SA_RESTART, SIGCHLD, kill, SIGPIPE)
 * local sockets (pairs, a named server, SCM_RIGHTS, datagrams) and shared
 * memory (memfd, MAP_SHARED across fork and over a socket, shm_open),
 * starting a child the way browsers do (dup2 into place, close the rest),
 * a process-shared semaphore, inotify, seccomp filters and prompt
 * wake-ups.
 * Built by scripts/test-linux.sh with the host's gcc. */
#define _GNU_SOURCE
#include <dirent.h>
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <gnu/libc-version.h>
#include <linux/audit.h>
#include <linux/filter.h>
#include <linux/futex.h>
#include <linux/seccomp.h>
#include <poll.h>
#include <pthread.h>
#include <semaphore.h>
#include <setjmp.h>
#include <signal.h>
#include <spawn.h>
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/epoll.h>
#include <sys/inotify.h>
#include <sys/mman.h>
#include <sys/prctl.h>
#include <sys/resource.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/un.h>
#include <sys/wait.h>
#include <time.h>
#include <ucontext.h>
#include <unistd.h>

extern char **environ;
static int global = 1;

static volatile sig_atomic_t got_usr1, got_usr2, got_chld, spin_stop, segv_hits;
static volatile pid_t info_pid;
static void *volatile segv_addr;
static sigjmp_buf segv_jump;
static int pipe_r;

static void on_usr1(int s) { (void)s; got_usr1++; }
static void on_usr2(int s) { (void)s; got_usr2++; }
static void on_chld(int s) { (void)s; got_chld++; }
static void on_spin(int s) { (void)s; spin_stop = 1; }

static void on_info(int s, siginfo_t *si, void *uc) {
    (void)s;
    (void)uc;
    info_pid = si->si_pid;
}

static void on_segv(int s, siginfo_t *si, void *uc) {
    (void)s;
    (void)uc;
    segv_addr = si->si_addr;
    segv_hits++;
    siglongjmp(segv_jump, 1);
}

/* Busy, without system calls, until a signal stops it. */
static void *spinner(void *arg) {
    (void)arg;
    volatile unsigned long n = 0;
    while (!spin_stop)
        n++;
    return NULL;
}

/* One byte from the pipe, or -errno. */
static void *reader(void *arg) {
    char c;
    long r = read(pipe_r, &c, 1);
    *(int *)arg = r < 0 ? -errno : (r == 1 ? c : 0);
    return NULL;
}

/* Sends descriptor `fd` over socket `s`, with one byte. */
static int send_fd(int s, int fd) {
    char c[CMSG_SPACE(sizeof(int))];
    memset(c, 0, sizeof c);
    struct iovec io = {(void *)"f", 1};
    struct msghdr m;
    memset(&m, 0, sizeof m);
    m.msg_iov = &io;
    m.msg_iovlen = 1;
    m.msg_control = c;
    m.msg_controllen = sizeof c;
    struct cmsghdr *h = CMSG_FIRSTHDR(&m);
    h->cmsg_level = SOL_SOCKET;
    h->cmsg_type = SCM_RIGHTS;
    h->cmsg_len = CMSG_LEN(sizeof(int));
    memcpy(CMSG_DATA(h), &fd, sizeof fd);
    return (int)sendmsg(s, &m, 0);
}

/* The descriptor that came over socket `s`, or -1. */
static int recv_fd(int s) {
    char b, c[CMSG_SPACE(sizeof(int))];
    struct iovec io = {&b, 1};
    struct msghdr m;
    memset(&m, 0, sizeof m);
    m.msg_iov = &io;
    m.msg_iovlen = 1;
    m.msg_control = c;
    m.msg_controllen = sizeof c;
    if (recvmsg(s, &m, 0) != 1)
        return -1;
    struct cmsghdr *h = CMSG_FIRSTHDR(&m);
    if (!h || h->cmsg_type != SCM_RIGHTS)
        return -1;
    int fd;
    memcpy(&fd, CMSG_DATA(h), sizeof fd);
    return fd;
}

/* Waits for a child and returns its exit code (-1 if it did not exit). */
static int reap(pid_t pid) {
    int st = 0;
    if (waitpid(pid, &st, 0) != pid)
        return -2;
    return WIFEXITED(st) ? WEXITSTATUS(st) : -1;
}

static long long now_ns(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return ts.tv_sec * 1000000000LL + ts.tv_nsec;
}

static int futex_word;
static volatile long long futex_sent;

static void *futex_waker(void *arg) {
    (void)arg;
    usleep(100000);
    futex_sent = now_ns();
    __atomic_store_n(&futex_word, 1, __ATOMIC_SEQ_CST);
    syscall(SYS_futex, &futex_word, FUTEX_WAKE_PRIVATE, 1, NULL, NULL, 0);
    return NULL;
}

static void *sock_writer(void *arg) {
    int fd = *(int *)arg;
    usleep(100000);
    long long t = now_ns();
    if (write(fd, &t, sizeof t) != sizeof t)
        return NULL;
    return NULL;
}

static void *worker(void *arg) {
    long *n = arg;
    for (int i = 0; i < 100000; i++)
        __atomic_add_fetch(n, 1, __ATOMIC_SEQ_CST);
    return NULL;
}

/* seccomp: the first filter fails getpid with 77, turns getppid into a
   SIGSYS (data 9), kills on uname and fails getpriority(_, 31337) with 33. */
static struct sock_filter sc_f1[] = {
    BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, arch)),
    BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, AUDIT_ARCH_X86_64, 1, 0),
    BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_KILL_PROCESS),
    BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, nr)),
    BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, __NR_getpid, 0, 1),
    BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ERRNO | 77),
    BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, __NR_getppid, 0, 1),
    BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_TRAP | 9),
    BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, __NR_uname, 0, 1),
    BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_KILL_PROCESS),
    BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, __NR_getpriority, 0, 3),
    BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, args[1])),
    BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, 31337, 0, 1),
    BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ERRNO | 33),
    BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
};

/* The second, on top: gettid fails with 66 (found with arithmetic, scratch
   memory and a jump on X), getpid with 88 (the newer of two errors). */
static struct sock_filter sc_f2[] = {
    BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, nr)),
    BPF_STMT(BPF_ST, 2),
    BPF_STMT(BPF_ALU | BPF_ADD | BPF_K, 1),
    BPF_STMT(BPF_ALU | BPF_MUL | BPF_K, 2),
    BPF_STMT(BPF_MISC | BPF_TAX, 0),
    BPF_STMT(BPF_LD | BPF_IMM, (__NR_gettid + 1) * 2),
    BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_X, 0, 0, 1),
    BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ERRNO | 66),
    BPF_STMT(BPF_LD | BPF_MEM, 2),
    BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, __NR_getpid, 0, 1),
    BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ERRNO | 88),
    BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
};

static volatile int sc_nr, sc_code, sc_data, sc_arch, sc_ip, sc_rax;
static int sc_go[2], sc_back[2];

static void on_sys(int s, siginfo_t *si, void *uc_) {
    (void)s;
    ucontext_t *uc = uc_;
    sc_nr = si->si_syscall;
    sc_code = si->si_code;
    sc_data = si->si_errno;
    sc_arch = si->si_arch == AUDIT_ARCH_X86_64;
    sc_ip = (greg_t)si->si_call_addr == uc->uc_mcontext.gregs[REG_RIP];
    sc_rax = uc->uc_mcontext.gregs[REG_RAX] == si->si_syscall;
    uc->uc_mcontext.gregs[REG_RAX] = 4242;
}

/* A thread made before the filters: TSYNC gives it the first, not the
   second. */
static void *sc_thread(void *arg) {
    int *res = arg;
    char c;
    if (read(sc_go[0], &c, 1) != 1)
        return NULL;
    errno = 0;
    res[0] = syscall(SYS_getpid) == -1 ? errno : 0;
    if (write(sc_back[1], "b", 1) != 1 || read(sc_go[0], &c, 1) != 1)
        return NULL;
    res[1] = syscall(SYS_gettid) > 0;
    return NULL;
}

int main(int argc, char **argv) {
    /* Started again by the tests below. */
    if (argc > 2 && strcmp(argv[1], "child") == 0) {
        printf("dt: child says %s home=%s\n", argv[2], getenv("HOME") ? getenv("HOME") : "-");
        return 3;
    }
    if (argc > 2 && strcmp(argv[1], "fdcheck") == 0) {
        /* The descriptor dup2'd into place before exec is still open, still
           the socket, and not close-on-exec. */
        int fd = atoi(argv[2]), buf = 0;
        socklen_t len = sizeof buf;
        int sock = getsockopt(fd, SOL_SOCKET, SO_SNDBUF, &buf, &len) == 0 && buf > 0;
        int cloexec = fcntl(fd, F_GETFD) & FD_CLOEXEC;
        if (write(fd, "k", 1) != 1)
            return 1;
        printf("dt: dup2 survives exec sock=%d cloexec=%d\n", sock, cloexec);
        return 5;
    }
    if (argc > 2 && strcmp(argv[1], "shebang") == 0) {
        printf("dt: shebang %s\n", argv[2]);
        return 4;
    }
    printf("dt: glibc %s\n", gnu_get_libc_version());

    long n = 0;
    pthread_t t[4];
    for (int i = 0; i < 4; i++)
        pthread_create(&t[i], NULL, worker, &n);
    for (int i = 0; i < 4; i++)
        pthread_join(t[i], NULL);
    printf("dt: threads %ld\n", n);

    void *m = dlopen("libm.so.6", RTLD_NOW);
    double (*cosine)(double) = m ? (double (*)(double))dlsym(m, "cos") : NULL;
    printf("dt: dlopen cos(0)=%.1f\n", cosine ? cosine(0.0) : -1.0);

    char *s = strdup(argv[0]);
    printf("dt: argv0 %s\n", s);
    free(s);

    /* fork: the child has a copy of the memory and the parent's pipe. */
    fflush(stdout);
    pid_t parent = getpid();
    int p[2];
    if (pipe(p) != 0)
        return 1;
    pid_t c = fork();
    if (c == 0) {
        global = 2;
        close(p[0]);
        const char *word = getppid() == parent ? "hello" : "orphan";
        if (write(p[1], word, strlen(word)) < 0)
            _exit(1);
        _exit(7);
    }
    close(p[1]);
    char buf[16] = {0};
    ssize_t got = read(p[0], buf, sizeof buf - 1);
    close(p[0]);
    printf("dt: fork pipe=%s exit=%d global=%d\n", got > 0 ? buf : "-", reap(c), global);

    char self[256];
    ssize_t k = readlink("/proc/self/exe", self, sizeof self - 1);
    self[k > 0 ? k : 0] = 0;

    /* vfork, then execve of this program. */
    fflush(stdout);
    char *vargs[] = {"dt", "child", "vforked", NULL};
    pid_t v = vfork();
    if (v == 0) {
        execve(self, vargs, environ);
        _exit(99);
    }
    printf("dt: vfork exit=%d\n", reap(v));

    /* posix_spawn: what most programs use to start others. */
    char *sargs[] = {"dt", "child", "spawned", NULL};
    pid_t sp = 0;
    fflush(stdout);
    int e = posix_spawn(&sp, self, NULL, NULL, sargs, environ);
    printf("dt: spawn err=%d exit=%d\n", e, e ? -3 : reap(sp));
    e = posix_spawn(&sp, "/no/such/program", NULL, NULL, sargs, environ);
    printf("dt: spawn missing=%d\n", e);

    /* A script naming its interpreter with #!. */
    FILE *f = fopen("dt-script", "w");
    if (f) {
        fprintf(f, "#!%s shebang\n", self);
        fclose(f);
        chmod("dt-script", 0755);
    }
    char *xargs[] = {"dt-script", NULL};
    fflush(stdout);
    e = posix_spawn(&sp, "./dt-script", NULL, NULL, xargs, environ);
    printf("dt: script err=%d exit=%d\n", e, e ? -3 : reap(sp));
    unlink("dt-script");

    errno = 0;
    pid_t none = waitpid(-1, NULL, WNOHANG);
    printf("dt: no children %d errno=%d\n", (int)none, errno);

    /* Signals: a handler, run when the signal is raised. */
    signal(SIGUSR1, on_usr1);
    raise(SIGUSR1);
    printf("dt: signal usr1=%d\n", (int)got_usr1);

    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_sigaction = on_info;
    sa.sa_flags = SA_SIGINFO;
    sigaction(SIGUSR2, &sa, NULL);
    kill(getpid(), SIGUSR2);
    printf("dt: siginfo pid=%d\n", info_pid == getpid());

    /* Blocked, it waits; unblocked, it arrives. */
    signal(SIGUSR2, on_usr2);
    sigset_t block, old, pend;
    sigemptyset(&block);
    sigaddset(&block, SIGUSR2);
    sigprocmask(SIG_BLOCK, &block, &old);
    raise(SIGUSR2);
    int before = got_usr2;
    sigpending(&pend);
    int pending = sigismember(&pend, SIGUSR2);
    sigprocmask(SIG_SETMASK, &old, NULL);
    printf("dt: blocked %d pending %d after %d\n", before, pending, (int)got_usr2);

    /* sigtimedwait takes a blocked signal without the handler. */
    sigprocmask(SIG_BLOCK, &block, &old);
    raise(SIGUSR2);
    struct timespec zero = {0, 0};
    siginfo_t si;
    int taken = sigtimedwait(&block, &si, &zero);
    sigprocmask(SIG_SETMASK, &old, NULL);
    printf("dt: sigtimedwait %d handler %d\n", taken, (int)got_usr2);

    /* A fault, caught by a SIGSEGV handler that jumps back. */
    memset(&sa, 0, sizeof sa);
    sa.sa_sigaction = on_segv;
    sa.sa_flags = SA_SIGINFO;
    sigaction(SIGSEGV, &sa, NULL);
    if (sigsetjmp(segv_jump, 1) == 0)
        *(volatile int *)16 = 1;
    printf("dt: segv caught %d addr=%p\n", (int)segv_hits, segv_addr);
    signal(SIGSEGV, SIG_DFL);

    /* A thread busy in a loop gets a signal too. */
    signal(SIGUSR1, on_spin);
    pthread_t spin;
    pthread_create(&spin, NULL, spinner, NULL);
    usleep(50000);
    pthread_kill(spin, SIGUSR1);
    pthread_join(spin, NULL);
    printf("dt: signal in a loop %d\n", (int)spin_stop);

    /* A blocking read: EINTR without SA_RESTART, started again with it. */
    int q[2];
    int res = 0;
    pthread_t rd;
    memset(&sa, 0, sizeof sa);
    sa.sa_handler = on_usr2;
    sigaction(SIGUSR2, &sa, NULL);
    if (pipe(q) != 0)
        return 1;
    pipe_r = q[0];
    pthread_create(&rd, NULL, reader, &res);
    usleep(50000);
    pthread_kill(rd, SIGUSR2);
    usleep(100000);
    if (write(q[1], "X", 1) < 0) /* so that a missed signal cannot hang */
        return 1;
    pthread_join(rd, NULL);
    printf("dt: read interrupted %d\n", res == -EINTR);
    close(q[0]);
    close(q[1]);
    sa.sa_flags = SA_RESTART;
    sigaction(SIGUSR2, &sa, NULL);
    if (pipe(q) != 0)
        return 1;
    pipe_r = q[0];
    res = 0;
    pthread_create(&rd, NULL, reader, &res);
    usleep(50000);
    pthread_kill(rd, SIGUSR2);
    usleep(50000);
    if (write(q[1], "R", 1) < 0)
        return 1;
    pthread_join(rd, NULL);
    printf("dt: read restarted %c\n", res > 0 ? res : '-');
    close(q[0]);
    close(q[1]);

    /* SIGCHLD when a child ends, waited for with sigsuspend. */
    signal(SIGCHLD, on_chld);
    sigset_t chld, empty;
    sigemptyset(&chld);
    sigaddset(&chld, SIGCHLD);
    sigprocmask(SIG_BLOCK, &chld, &old);
    fflush(stdout);
    pid_t c2 = fork();
    if (c2 == 0)
        _exit(5);
    sigemptyset(&empty);
    int sus = sigsuspend(&empty);
    int sus_eintr = sus == -1 && errno == EINTR;
    sigprocmask(SIG_SETMASK, &old, NULL);
    printf("dt: sigchld %d suspend %d exit=%d\n", (int)got_chld, sus_eintr, reap(c2));
    signal(SIGCHLD, SIG_DFL);

    /* Children ended by a signal: kill from the parent, and abort. */
    fflush(stdout);
    pid_t c3 = fork();
    if (c3 == 0) {
        for (;;)
            pause();
    }
    usleep(50000);
    kill(c3, SIGTERM);
    int st3 = 0;
    waitpid(c3, &st3, 0);
    fflush(stdout);
    pid_t c4 = fork();
    if (c4 == 0)
        abort();
    int st4 = 0;
    waitpid(c4, &st4, 0);
    printf("dt: killed by %d, abort %d\n", WIFSIGNALED(st3) ? WTERMSIG(st3) : -1, WIFSIGNALED(st4) ? WTERMSIG(st4) : -1);

    /* Writing to a pipe nobody reads: SIGPIPE ends the writer. */
    fflush(stdout);
    pid_t c8 = fork();
    if (c8 == 0) {
        int sp[2];
        if (pipe(sp) != 0)
            _exit(1);
        close(sp[0]);
        if (write(sp[1], "x", 1) < 0)
            _exit(2);
        _exit(3);
    }
    int st8 = 0;
    waitpid(c8, &st8, 0);
    printf("dt: sigpipe %d\n", WIFSIGNALED(st8) ? WTERMSIG(st8) : -WEXITSTATUS(st8));

    /* Local sockets: a pair across fork, both ways. */
    int sv[2];
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, sv) != 0)
        return 1;
    fflush(stdout);
    pid_t c5 = fork();
    if (c5 == 0) {
        close(sv[0]);
        char b[8] = {0};
        if (read(sv[1], b, 4) != 4)
            _exit(1);
        if (write(sv[1], strcmp(b, "ping") == 0 ? "pong" : "bad!", 4) != 4)
            _exit(2);
        _exit(0);
    }
    close(sv[1]);
    if (write(sv[0], "ping", 4) != 4)
        return 1;
    char pb[8] = {0};
    int pn = (int)read(sv[0], pb, sizeof pb - 1);
    printf("dt: socketpair %s %d exit=%d\n", pb, pn, reap(c5));
    close(sv[0]);

    /* A descriptor passed along (SCM_RIGHTS): the child writes into a pipe
       the parent made after the fork. */
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, sv) != 0)
        return 1;
    fflush(stdout);
    pid_t c6 = fork();
    if (c6 == 0) {
        close(sv[0]);
        char b[4];
        char cbuf[CMSG_SPACE(sizeof(int))];
        struct iovec io = {b, sizeof b};
        struct msghdr mh;
        memset(&mh, 0, sizeof mh);
        mh.msg_iov = &io;
        mh.msg_iovlen = 1;
        mh.msg_control = cbuf;
        mh.msg_controllen = sizeof cbuf;
        if (recvmsg(sv[1], &mh, 0) != 1)
            _exit(1);
        struct cmsghdr *cm = CMSG_FIRSTHDR(&mh);
        if (!cm || cm->cmsg_level != SOL_SOCKET || cm->cmsg_type != SCM_RIGHTS)
            _exit(2);
        int got;
        memcpy(&got, CMSG_DATA(cm), sizeof got);
        if (write(got, "hello via fd", 12) != 12)
            _exit(3);
        _exit(0);
    }
    close(sv[1]);
    int fp[2];
    if (pipe(fp) != 0)
        return 1;
    char cb2[CMSG_SPACE(sizeof(int))];
    memset(cb2, 0, sizeof cb2);
    struct iovec io2 = {(void *)"x", 1};
    struct msghdr m2;
    memset(&m2, 0, sizeof m2);
    m2.msg_iov = &io2;
    m2.msg_iovlen = 1;
    m2.msg_control = cb2;
    m2.msg_controllen = sizeof cb2;
    struct cmsghdr *cm2 = CMSG_FIRSTHDR(&m2);
    cm2->cmsg_level = SOL_SOCKET;
    cm2->cmsg_type = SCM_RIGHTS;
    cm2->cmsg_len = CMSG_LEN(sizeof(int));
    memcpy(CMSG_DATA(cm2), &fp[1], sizeof(int));
    long sent = (long)sendmsg(sv[0], &m2, 0);
    close(fp[1]);
    char fbuf[32] = {0};
    long fr = (long)read(fp[0], fbuf, sizeof fbuf - 1);
    printf("dt: fd passed %ld %ld %s exit=%d\n", sent, fr, fbuf, reap(c6));
    close(fp[0]);
    close(sv[0]);

    /* A named server: bind, listen, a child connects, accept. */
    unlink("dt.sock");
    struct sockaddr_un ua;
    memset(&ua, 0, sizeof ua);
    ua.sun_family = AF_UNIX;
    strcpy(ua.sun_path, "dt.sock");
    int srv = socket(AF_UNIX, SOCK_STREAM, 0);
    int b1 = bind(srv, (struct sockaddr *)&ua, sizeof ua);
    int l1 = listen(srv, 4);
    fflush(stdout);
    pid_t c7 = fork();
    if (c7 == 0) {
        int cs = socket(AF_UNIX, SOCK_STREAM, 0);
        if (connect(cs, (struct sockaddr *)&ua, sizeof ua) != 0)
            _exit(1);
        if (write(cs, "hi", 2) != 2)
            _exit(2);
        char r[4] = {0};
        if (read(cs, r, 2) != 2)
            _exit(3);
        _exit(strcmp(r, "ok") == 0 ? 7 : 4);
    }
    struct sockaddr_un pa;
    socklen_t pl = sizeof pa;
    int acc = accept(srv, (struct sockaddr *)&pa, &pl);
    struct ucred cr;
    memset(&cr, 0, sizeof cr);
    socklen_t crl = sizeof cr;
    getsockopt(acc, SOL_SOCKET, SO_PEERCRED, &cr, &crl);
    char hb[4] = {0};
    if (read(acc, hb, 2) != 2)
        return 1;
    if (write(acc, "ok", 2) != 2)
        return 1;
    struct stat sst;
    int issock = stat("dt.sock", &sst) == 0 && S_ISSOCK(sst.st_mode);
    int e7 = reap(c7);
    printf("dt: unix server %d %d peerlen=%d cred=%d got %s exit=%d sock=%d\n", b1, l1, (int)pl, cr.pid == c7, hb, e7, issock);

    /* The name is taken until it is unlinked. */
    int s2 = socket(AF_UNIX, SOCK_STREAM, 0);
    int busy = bind(s2, (struct sockaddr *)&ua, sizeof ua) == 0 ? 0 : errno;
    int ul = unlink("dt.sock");
    int again = bind(s2, (struct sockaddr *)&ua, sizeof ua);
    printf("dt: unix bind busy %d unlink %d again %d\n", busy, ul, again);
    close(s2);
    close(acc);
    close(srv);

    /* Nobody there: refused (abstract name), or no such file (path). */
    struct sockaddr_un na;
    memset(&na, 0, sizeof na);
    na.sun_family = AF_UNIX;
    memcpy(na.sun_path, "\0dt-nobody", 10);
    int s3 = socket(AF_UNIX, SOCK_STREAM, 0);
    int r1 = connect(s3, (struct sockaddr *)&na, sizeof(sa_family_t) + 10) == 0 ? 0 : errno;
    strcpy(ua.sun_path, "dt-missing.sock");
    int r2 = connect(s3, (struct sockaddr *)&ua, sizeof ua) == 0 ? 0 : errno;
    close(s3);
    printf("dt: unix refused %d missing %d\n", r1, r2);

    /* Message boundaries: seqpacket keeps them, a short buffer cuts a datagram. */
    if (socketpair(AF_UNIX, SOCK_SEQPACKET, 0, sv) != 0)
        return 1;
    send(sv[0], "ab", 2, 0);
    send(sv[0], "cde", 3, 0);
    char mb[16];
    int q1 = (int)recv(sv[1], mb, sizeof mb, 0);
    int q2 = (int)recv(sv[1], mb, sizeof mb, 0);
    close(sv[0]);
    close(sv[1]);
    if (socketpair(AF_UNIX, SOCK_DGRAM, 0, sv) != 0)
        return 1;
    send(sv[0], "hello", 5, 0);
    struct iovec io3 = {mb, 2};
    struct msghdr m3;
    memset(&m3, 0, sizeof m3);
    m3.msg_iov = &io3;
    m3.msg_iovlen = 1;
    int q3 = (int)recvmsg(sv[1], &m3, 0);
    printf("dt: seqpacket %d %d dgram %d trunc %d\n", q1, q2, q3, (m3.msg_flags & MSG_TRUNC) != 0);
    close(sv[0]);
    close(sv[1]);

    /* Datagrams between two named sockets, in the abstract namespace. */
    struct sockaddr_un da, db, from;
    socklen_t dl = sizeof(sa_family_t) + 5;
    memset(&da, 0, sizeof da);
    memset(&db, 0, sizeof db);
    da.sun_family = db.sun_family = AF_UNIX;
    memcpy(da.sun_path, "\0dt-a", 5);
    memcpy(db.sun_path, "\0dt-b", 5);
    int ga = socket(AF_UNIX, SOCK_DGRAM, 0);
    int gb = socket(AF_UNIX, SOCK_DGRAM, 0);
    bind(ga, (struct sockaddr *)&da, dl);
    bind(gb, (struct sockaddr *)&db, dl);
    sendto(gb, "to a", 4, 0, (struct sockaddr *)&da, dl);
    socklen_t froml = sizeof from;
    memset(&from, 0, sizeof from);
    int gn = (int)recvfrom(ga, mb, sizeof mb, 0, (struct sockaddr *)&from, &froml);
    printf("dt: dgram %d from @%s len=%d\n", gn, from.sun_path + 1, (int)froml);
    close(ga);
    close(gb);

    /* The other end: EAGAIN while it is there, then the end, EPIPE, POLLHUP. */
    if (socketpair(AF_UNIX, SOCK_STREAM | SOCK_NONBLOCK, 0, sv) != 0)
        return 1;
    int ea = read(sv[0], mb, sizeof mb) < 0 ? errno : 0;
    close(sv[1]);
    int eof = (int)read(sv[0], mb, sizeof mb);
    int ep = send(sv[0], "x", 1, MSG_NOSIGNAL) < 0 ? errno : 0;
    struct pollfd pf = {sv[0], POLLIN, 0};
    poll(&pf, 1, 0);
    printf("dt: unix eagain %d eof %d epipe %d hup %d\n", ea, eof, ep, (pf.revents & POLLHUP) != 0);
    close(sv[0]);

    /* Shared memory: a memfd, mapped, written by a child, sealed. */
    int mf = memfd_create("dt", MFD_CLOEXEC | MFD_ALLOW_SEALING);
    if (mf < 0 || ftruncate(mf, 8192) != 0)
        return 1;
    char *sm = mmap(NULL, 8192, PROT_READ | PROT_WRITE, MAP_SHARED, mf, 0);
    if (sm == MAP_FAILED)
        return 1;
    strcpy(sm, "shared");
    char prd[8] = {0};
    if (pread(mf, prd, 6, 0) != 6)
        return 1;
    fflush(stdout);
    pid_t c9 = fork();
    if (c9 == 0) {
        strcpy(sm + 4096, "child was here");
        _exit(0);
    }
    reap(c9);
    fcntl(mf, F_ADD_SEALS, F_SEAL_SHRINK);
    int sealed = ftruncate(mf, 4096) != 0 && errno == EPERM;
    char lk[64] = {0}, lp[32];
    snprintf(lp, sizeof lp, "/proc/self/fd/%d", mf);
    if (readlink(lp, lk, sizeof lk - 1) < 0)
        strcpy(lk, "?");
    printf("dt: memfd %s pread=%s child=%s seal=%d link=%s\n", sm, prd, sm + 4096, sealed, lk);
    munmap(sm, 8192);
    close(mf);

    /* MAP_SHARED | MAP_ANONYMOUS: a counter the child sets. */
    int *cnt = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    if (cnt == MAP_FAILED)
        return 1;
    *cnt = 1;
    fflush(stdout);
    pid_t c10 = fork();
    if (c10 == 0) {
        *cnt = 42;
        _exit(0);
    }
    reap(c10);
    printf("dt: shared anon %d\n", *cnt);
    munmap(cnt, 4096);

    /* The Wayland way: a memfd made after the fork goes to the child over a
       socket; the child maps it and draws into it. */
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, sv) != 0)
        return 1;
    fflush(stdout);
    pid_t c11 = fork();
    if (c11 == 0) {
        close(sv[0]);
        int got = recv_fd(sv[1]);
        if (got < 0)
            _exit(1);
        char *p = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, got, 0);
        if (p == MAP_FAILED)
            _exit(2);
        strcpy(p, "from the child");
        if (write(sv[1], "d", 1) != 1)
            _exit(3);
        _exit(0);
    }
    close(sv[1]);
    int mf2 = memfd_create("dt-pool", 0);
    if (mf2 < 0 || ftruncate(mf2, 4096) != 0)
        return 1;
    char *pool = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, mf2, 0);
    if (pool == MAP_FAILED || send_fd(sv[0], mf2) != 1)
        return 1;
    char dn;
    if (read(sv[0], &dn, 1) != 1)
        return 1;
    printf("dt: memfd passed %s exit=%d\n", pool, reap(c11));
    munmap(pool, 4096);
    close(mf2);
    close(sv[0]);

    /* shm_open: named shared memory in /dev/shm, until shm_unlink. */
    int so = shm_open("/dt-shm", O_CREAT | O_RDWR, 0600);
    int ok1 = so >= 0 && ftruncate(so, 4096) == 0 && write(so, "hello", 5) == 5;
    close(so);
    so = shm_open("/dt-shm", O_RDWR, 0);
    char sb[8] = {0};
    int ok2 = so >= 0 && pread(so, sb, 5, 0) == 5;
    close(so);
    shm_unlink("/dt-shm");
    int gone = shm_open("/dt-shm", O_RDWR, 0) < 0 ? errno : 0;
    printf("dt: shm_open %s %d%d again %d\n", sb, ok1, ok2, gone);

    /* Starting a child the way browsers do: as many descriptors as
       getrlimit says, the channel dup2'd into place (the original is
       close-on-exec), the rest listed in /proc/self/fd or closed with
       close_range. */
    struct rlimit rl;
    getrlimit(RLIMIT_NOFILE, &rl);
    printf("dt: nofile finite=%d\n", rl.rlim_cur != RLIM_INFINITY && rl.rlim_cur > 0);
    if (socketpair(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0, sv) != 0)
        return 1;
    fflush(stdout);
    pid_t c12 = fork();
    if (c12 == 0) {
        char *fargs[] = {"dt", "fdcheck", "9", NULL};
        if (dup2(sv[1], 9) != 9)
            _exit(1);
        execve(self, fargs, environ);
        _exit(2);
    }
    close(sv[1]);
    char ck = 0;
    if (read(sv[0], &ck, 1) != 1)
        ck = '-';
    printf("dt: dup2 exec got %c exit=%d\n", ck, reap(c12));
    close(sv[0]);
    int listed50 = 0, listed1 = 0;
    if (dup2(1, 50) == 50) {
        DIR *dir = opendir("/proc/self/fd");
        struct dirent *de;
        while (dir && (de = readdir(dir)) != NULL) {
            listed50 |= strcmp(de->d_name, "50") == 0;
            listed1 |= strcmp(de->d_name, "1") == 0;
        }
        if (dir)
            closedir(dir);
    }
    dup2(1, 51);
    long ranged = syscall(SYS_close_range, 50, ~0U, 0);
    int closed = fcntl(50, F_GETFD) < 0 && errno == EBADF && fcntl(51, F_GETFD) < 0;
    printf("dt: proc fd 50=%d 1=%d close_range %ld closed=%d\n", listed50, listed1, ranged, closed);

    /* A semaphore in memory both processes share: the child waits in the
       kernel until the parent posts. */
    sem_t *sem = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    if (sem == MAP_FAILED || sem_init(sem, 1, 0) != 0)
        return 1;
    fflush(stdout);
    pid_t c13 = fork();
    if (c13 == 0) {
        sem_wait(sem);
        _exit(6);
    }
    usleep(300000);
    sem_post(sem);
    printf("dt: pshared semaphore exit=%d\n", reap(c13));

    /* inotify: a watched folder reports a file created, written, closed,
       renamed (both halves with one cookie) and deleted, in that order; then
       nothing; the watch goes with IN_IGNORED. A watch on the file itself
       follows it through the rename and reports its deletion. */
    mkdir("ino", 0755);
    int in = inotify_init1(IN_NONBLOCK | IN_CLOEXEC);
    int wdir = inotify_add_watch(in, "ino", IN_CREATE | IN_MODIFY | IN_CLOSE_WRITE | IN_MOVED_FROM | IN_MOVED_TO | IN_DELETE);
    int wf = open("ino/a", O_CREAT | O_WRONLY, 0644);
    int wfile = inotify_add_watch(in, "ino/a", IN_DELETE_SELF | IN_MOVE_SELF);
    write(wf, "x", 1);
    close(wf);
    rename("ino/a", "ino/b");
    struct pollfd ipf = {in, POLLIN, 0};
    int ipoll = poll(&ipf, 1, 1000);
    unlink("ino/b");
    char ibuf[4096] __attribute__((aligned(8)));
    char iseen[256] = "";
    int igot = 0;
    uint32_t c_from = 0, c_to = 1;
    for (int tries = 0; tries < 2; tries++) {
        ssize_t n = read(in, ibuf, sizeof ibuf);
        for (ssize_t at = 0; n > 0 && at < n;) {
            struct inotify_event *e = (struct inotify_event *)(ibuf + at);
            const char *tag = e->mask & IN_CREATE ? "C" : e->mask & IN_MODIFY ? "M" : e->mask & IN_CLOSE_WRITE ? "W"
                : e->mask & IN_MOVED_FROM ? "F" : e->mask & IN_MOVED_TO ? "T" : e->mask & IN_DELETE ? "D"
                : e->mask & IN_MOVE_SELF ? "m" : e->mask & IN_DELETE_SELF ? "d" : e->mask & IN_IGNORED ? "i" : "?";
            if (e->mask & IN_MOVED_FROM)
                c_from = e->cookie;
            if (e->mask & IN_MOVED_TO)
                c_to = e->cookie;
            strcat(iseen, tag);
            if (e->len)
                strcat(iseen, e->name);
            strcat(iseen, e->wd == wdir ? "" : e->wd == wfile ? "'" : "?");
            igot++;
            at += sizeof *e + e->len;
        }
    }
    int iagain = read(in, ibuf, sizeof ibuf) < 0 && errno == EAGAIN;
    int irm = inotify_rm_watch(in, wdir);
    ssize_t ilast = read(in, ibuf, sizeof ibuf);
    int iignored = ilast >= (ssize_t)sizeof(struct inotify_event) && (((struct inotify_event *)ibuf)->mask & IN_IGNORED);
    printf("dt: inotify %s cookie=%d poll=%d again=%d rm=%d ignored=%d\n", iseen, c_from == c_to && c_from != 0, ipoll, iagain, irm, iignored);
    close(in);
    rmdir("ino");

    /* seccomp, in a child: the probe Firefox makes, no_new_privs, two
       filters (see sc_f1 and sc_f2), the SIGSYS a filter asks for and what
       its handler sees, TSYNC; then a grandchild killed by the filter, and
       strict mode in another child (write goes through, getpid kills). */
    fflush(stdout);
    pid_t c14 = fork();
    if (c14 == 0) {
        struct sigaction sa;
        memset(&sa, 0, sizeof sa);
        sa.sa_sigaction = on_sys;
        sa.sa_flags = SA_SIGINFO;
        sigaction(SIGSYS, &sa, NULL);
        int tres[2] = {-1, -1};
        if (pipe(sc_go) != 0 || pipe(sc_back) != 0)
            _exit(1);
        pthread_t th;
        pthread_create(&th, NULL, sc_thread, tres);
        int probe = prctl(PR_SET_SECCOMP, SECCOMP_MODE_FILTER, NULL) == -1 && errno == EFAULT;
        int nnp = prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) == 0 && prctl(PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0) == 1;
        struct sock_fprog p1 = {sizeof sc_f1 / sizeof sc_f1[0], sc_f1};
        struct sock_fprog p2 = {sizeof sc_f2 / sizeof sc_f2[0], sc_f2};
        long set1 = syscall(SYS_seccomp, SECCOMP_SET_MODE_FILTER, SECCOMP_FILTER_FLAG_TSYNC, &p1);
        char c;
        if (write(sc_go[1], "g", 1) != 1 || read(sc_back[0], &c, 1) != 1)
            _exit(1);
        long set2 = prctl(PR_SET_SECCOMP, SECCOMP_MODE_FILTER, &p2);
        if (write(sc_go[1], "g", 1) != 1)
            _exit(1);
        pthread_join(th, NULL);
        errno = 0;
        long gp = syscall(SYS_getpid);
        int gpe = errno;
        long gpp = syscall(SYS_getppid);
        errno = 0;
        syscall(SYS_gettid);
        int gte = errno;
        errno = 0;
        syscall(SYS_getpriority, PRIO_PROCESS, 31337);
        int pre = errno;
        long pr = syscall(SYS_getpriority, PRIO_PROCESS, 0);
        int mode = prctl(PR_GET_SECCOMP, 0, 0, 0, 0);
        printf("dt: seccomp probe=%d nnp=%d set=%ld,%ld getpid=%ld/%d getppid=%ld sys=%d,%d,%d,%d,%d,%d gettid=%d prio=%d,%ld thread=%d,%d mode=%d\n",
               probe, nnp, set1, set2, gp, gpe, gpp, sc_nr, sc_code, sc_data, sc_arch, sc_ip, sc_rax, gte, pre, pr, tres[0], tres[1], mode);
        fflush(stdout);
        pid_t k = fork();
        if (k == 0) {
            syscall(SYS_uname, NULL);
            _exit(0);
        }
        int ks = 0;
        waitpid(k, &ks, 0);
        printf("dt: seccomp kill %d\n", WIFSIGNALED(ks) ? WTERMSIG(ks) : -1);
        fflush(stdout);
        _exit(0);
    }
    int sc_exit = reap(c14);
    pid_t c15 = fork();
    if (c15 == 0) {
        if (syscall(SYS_seccomp, SECCOMP_SET_MODE_STRICT, 0, NULL) != 0)
            _exit(1);
        static const char ok[] = "dt: strict write\n";
        syscall(SYS_write, 1, ok, sizeof ok - 1);
        syscall(SYS_getpid);
        syscall(SYS_exit, 3);
    }
    int ss = 0;
    waitpid(c15, &ss, 0);
    printf("dt: seccomp exit=%d strict %d\n", sc_exit, WIFSIGNALED(ss) ? WTERMSIG(ss) : -1);

    /* Waking is prompt: a process waiting in poll on a pipe, a thread in
       epoll_wait on a local socket and one on a futex wake when the other
       side acts, not at some later look. Each is measured from the moment
       of the write (the worst of a few rounds). */
    long long worst_pipe = 0, worst_unix = 0, worst_futex = 0, stamp = 0, d;
    for (int round = 0; round < 4; round++) {
        int wp[2];
        if (pipe(wp) != 0)
            return 1;
        fflush(stdout);
        pid_t c = fork();
        if (c == 0) {
            usleep(100000);
            long long t = now_ns();
            _exit(write(wp[1], &t, sizeof t) == sizeof t ? 0 : 1);
        }
        struct pollfd pf = {wp[0], POLLIN, 0};
        if (poll(&pf, 1, 5000) == 1 && read(wp[0], &stamp, sizeof stamp) == sizeof stamp)
            d = now_ns() - stamp;
        else
            d = 1LL << 40;
        worst_pipe = d > worst_pipe ? d : worst_pipe;
        reap(c);
        close(wp[0]);
        close(wp[1]);

        int sp[2];
        if (socketpair(AF_UNIX, SOCK_STREAM, 0, sp) != 0)
            return 1;
        int ep = epoll_create1(0);
        struct epoll_event ev = {.events = EPOLLIN, .data.fd = sp[0]}, got;
        epoll_ctl(ep, EPOLL_CTL_ADD, sp[0], &ev);
        pthread_t th;
        pthread_create(&th, NULL, sock_writer, &sp[1]);
        if (epoll_wait(ep, &got, 1, 5000) == 1 && read(sp[0], &stamp, sizeof stamp) == sizeof stamp)
            d = now_ns() - stamp;
        else
            d = 1LL << 40;
        worst_unix = d > worst_unix ? d : worst_unix;
        pthread_join(th, NULL);
        close(ep);
        close(sp[0]);
        close(sp[1]);

        futex_word = 0;
        pthread_create(&th, NULL, futex_waker, NULL);
        while (__atomic_load_n(&futex_word, __ATOMIC_SEQ_CST) == 0)
            syscall(SYS_futex, &futex_word, FUTEX_WAIT_PRIVATE, 0, NULL, NULL, 0);
        d = now_ns() - futex_sent;
        worst_futex = d > worst_futex ? d : worst_futex;
        pthread_join(th, NULL);
    }
    const long long fast = 100000000; /* 100 ms */
    printf("dt: prompt wake pipe=%d unix=%d futex=%d\n", worst_pipe < fast, worst_unix < fast, worst_futex < fast);
    if (getenv("DT_WAKE_MS"))
        printf("dt: wake ms pipe=%lld unix=%lld futex=%lld\n", worst_pipe / 1000000, worst_unix / 1000000, worst_futex / 1000000);
    return 0;
}
