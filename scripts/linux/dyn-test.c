/* A dynamically linked glibc program for the Linux layer: the dynamic
 * loader, shared libraries, threads in libc, dlopen, and new processes
 * (fork, vfork, posix_spawn, execve of a #! script, waitpid).
 * Built by scripts/test-linux.sh with the host's gcc. */
#include <dlfcn.h>
#include <errno.h>
#include <gnu/libc-version.h>
#include <pthread.h>
#include <spawn.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>

extern char **environ;
static int global = 1;

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
    return 0;
}
