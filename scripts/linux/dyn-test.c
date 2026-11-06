/* A dynamically linked glibc program for the Linux layer: the dynamic
 * loader, shared libraries, threads in libc and dlopen.
 * Built by scripts/test-linux.sh with the host's gcc. */
#include <dlfcn.h>
#include <gnu/libc-version.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static void *worker(void *arg) {
    long *n = arg;
    for (int i = 0; i < 100000; i++)
        __atomic_add_fetch(n, 1, __ATOMIC_SEQ_CST);
    return NULL;
}

int main(int argc, char **argv) {
    (void)argc;
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
    return 0;
}
