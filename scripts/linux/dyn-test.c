/* A dynamically linked glibc program for the Linux layer: the dynamic
 * loader, shared libraries, threads in libc, dlopen, new processes
 * (fork, vfork, posix_spawn, execve of a #! script, waitpid) and signals
 * (handlers, masks, faults, EINTR and SA_RESTART, SIGCHLD, kill).
 * Built by scripts/test-linux.sh with the host's gcc. */
#include <dlfcn.h>
#include <errno.h>
#include <gnu/libc-version.h>
#include <pthread.h>
#include <setjmp.h>
#include <signal.h>
#include <spawn.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/wait.h>
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

/* Waits for a child and returns its exit code (-1 if it did not exit). */
static int reap(pid_t pid) {
    int st = 0;
    if (waitpid(pid, &st, 0) != pid)
        return -2;
    return WIFEXITED(st) ? WEXITSTATUS(st) : -1;
}

static void *worker(void *arg) {
    long *n = arg;
    for (int i = 0; i < 100000; i++)
        __atomic_add_fetch(n, 1, __ATOMIC_SEQ_CST);
    return NULL;
}

int main(int argc, char **argv) {
    /* Started again by the tests below. */
    if (argc > 2 && strcmp(argv[1], "child") == 0) {
        printf("dt: child says %s home=%s\n", argv[2], getenv("HOME") ? getenv("HOME") : "-");
        return 3;
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
    return 0;
}
