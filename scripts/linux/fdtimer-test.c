/* timerfd and signalfd, as Android's init and systemd use them in their
 * epoll loops: a timer that goes off once and then periodically, with an
 * absolute wall-clock time too; SIGCHLD and other blocked signals read as
 * records, waking epoll and a blocking read.
 * Prints "tf:" and "sf:" lines for scripts/test-linux.sh. */
#include <errno.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/epoll.h>
#include <sys/signalfd.h>
#include <sys/timerfd.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static long ms_since(const struct timespec *a) {
    struct timespec b;
    clock_gettime(CLOCK_MONOTONIC, &b);
    return (b.tv_sec - a->tv_sec) * 1000 + (b.tv_nsec - a->tv_nsec) / 1000000;
}

static int timers(void) {
    int t = timerfd_create(CLOCK_MONOTONIC, TFD_CLOEXEC | TFD_NONBLOCK);
    if (t < 0) {
        printf("tf: create failed errno=%d\n", errno);
        return 1;
    }
    uint64_t n = 0;
    int eagain = read(t, &n, 8) < 0 && errno == EAGAIN;

    /* Once after 50 ms, then every 20 ms; epoll wakes for it. */
    struct itimerspec its = {{0, 20 * 1000000}, {0, 50 * 1000000}};
    timerfd_settime(t, 0, &its, NULL);
    int ep = epoll_create1(EPOLL_CLOEXEC);
    struct epoll_event ev = {.events = EPOLLIN, .data.u32 = 7}, out;
    epoll_ctl(ep, EPOLL_CTL_ADD, t, &ev);
    struct timespec start;
    clock_gettime(CLOCK_MONOTONIC, &start);
    int got = epoll_wait(ep, &out, 1, 2000);
    long waited = ms_since(&start);
    n = 0;
    read(t, &n, 8);
    struct itimerspec cur;
    timerfd_gettime(t, &cur);
    printf("tf: armed ready=%d ticks=%lu waited=%d interval=%ld eagain=%d\n", got == 1 && out.data.u32 == 7, (unsigned long)n,
           waited >= 45 && waited < 1500, cur.it_interval.tv_nsec / 1000000, eagain);

    usleep(150 * 1000);
    n = 0;
    read(t, &n, 8);
    int more = n >= 2;
    /* Off again: nothing more. */
    struct itimerspec off = {{0, 0}, {0, 0}};
    timerfd_settime(t, 0, &off, NULL);
    int quiet = epoll_wait(ep, &out, 1, 100) == 0;

    /* A blocking timer at an absolute time on the wall clock. */
    int a = timerfd_create(CLOCK_REALTIME, TFD_CLOEXEC);
    struct timespec now;
    clock_gettime(CLOCK_REALTIME, &now);
    now.tv_nsec += 80 * 1000000;
    if (now.tv_nsec >= 1000000000) {
        now.tv_sec++;
        now.tv_nsec -= 1000000000;
    }
    struct itimerspec at = {{0, 0}, now};
    timerfd_settime(a, TFD_TIMER_ABSTIME, &at, NULL);
    clock_gettime(CLOCK_MONOTONIC, &start);
    n = 0;
    read(a, &n, 8);
    waited = ms_since(&start);
    printf("tf: periodic more=%d quiet=%d abs=%lu late=%d\n", more, quiet, (unsigned long)n, waited < 1500);
    return 0;
}

static int signals(void) {
    sigset_t mask;
    sigemptyset(&mask);
    sigaddset(&mask, SIGCHLD);
    sigaddset(&mask, SIGUSR1);
    sigaddset(&mask, SIGUSR2);
    sigprocmask(SIG_BLOCK, &mask, NULL);
    int s = signalfd(-1, &mask, SFD_CLOEXEC);
    if (s < 0) {
        printf("sf: create failed errno=%d\n", errno);
        return 1;
    }
    int ep = epoll_create1(EPOLL_CLOEXEC);
    struct epoll_event ev = {.events = EPOLLIN, .data.u32 = 9}, out;
    epoll_ctl(ep, EPOLL_CTL_ADD, s, &ev);

    /* A child ends: SIGCHLD with its status, epoll wakes. */
    pid_t pid = fork();
    if (pid == 0)
        _exit(7);
    int got = epoll_wait(ep, &out, 1, 3000);
    struct signalfd_siginfo si;
    memset(&si, 0, sizeof si);
    ssize_t r = read(s, &si, sizeof si);
    printf("sf: chld epoll=%d size=%d signo=%u status=%d pidok=%d\n", got == 1, (int)r, si.ssi_signo, si.ssi_status,
           si.ssi_pid == (uint32_t)pid);
    waitpid(pid, NULL, 0);

    /* A signal to itself: read, not delivered. */
    kill(getpid(), SIGUSR1);
    memset(&si, 0, sizeof si);
    read(s, &si, sizeof si);
    printf("sf: usr1 signo=%u code=%d pidok=%d\n", si.ssi_signo, si.ssi_code, si.ssi_pid == (uint32_t)getpid());

    /* A blocking read that another process's signal ends. */
    pid = fork();
    if (pid == 0) {
        usleep(150 * 1000);
        kill(getppid(), SIGUSR2);
        _exit(0);
    }
    memset(&si, 0, sizeof si);
    read(s, &si, sizeof si);
    int usr2 = si.ssi_signo == SIGUSR2;
    waitpid(pid, NULL, 0);
    /* Its SIGCHLD is there too; a narrower mask leaves it. */
    sigset_t only;
    sigemptyset(&only);
    sigaddset(&only, SIGUSR1);
    signalfd(s, &only, 0);
    int left = epoll_wait(ep, &out, 1, 50) == 0;
    signalfd(s, &mask, 0);
    int back = epoll_wait(ep, &out, 1, 1000) == 1;
    printf("sf: done usr2=%d left=%d back=%d\n", usr2, left, back);
    return 0;
}

int main(void) {
    return timers() | signals();
}
