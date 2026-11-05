/* A Windows console program for scripts/test-compat.sh: files through the C
 * runtime and through Win32, strings, sorting, maths, formatting, and a
 * number read from the keyboard. */
#include <ctype.h>
#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <windows.h>

static int cmp(const void *a, const void *b) { return *(const int *)a - *(const int *)b; }

int main(void) {
    FILE *f = fopen("wtest.txt", "w");
    fprintf(f, "line one\n%d %s %.2f\n", 42, "word", 3.5);
    fclose(f);

    f = fopen("wtest.txt", "r");
    char buf[64];
    fgets(buf, sizeof buf, f);
    buf[strcspn(buf, "\n")] = 0;
    printf("wtest: first line '%s'\n", buf);
    int n;
    char w[16];
    double d;
    if (fscanf(f, "%d %15s %lf", &n, w, &d) == 3) printf("wtest: scanned %d %s %.2f\n", n, w, d);
    fseek(f, 0, SEEK_END);
    printf("wtest: size %ld\n", ftell(f));
    fclose(f);

    int v[] = {5, 3, 9, 1, 7};
    qsort(v, 5, sizeof(int), cmp);
    printf("wtest: sorted %d %d %d %d %d\n", v[0], v[1], v[2], v[3], v[4]);

    char s[64];
    strcpy(s, "Hello");
    strcat(s, ", JustOS");
    for (char *p = s; *p; p++) *p = toupper((unsigned char)*p);
    printf("wtest: %s %d\n", s, (int)strlen(s));

    printf("wtest: sqrt2=%.4f sin1=%.3f pow=%.0f neg=%d\n", sqrt(2.0), sin(1.0), pow(2.0, 10.0), -17);
    printf("wtest: strtol %ld atoi %d\n", strtol("0x1F", NULL, 16), atoi("-250"));
    char out[32];
    snprintf(out, sizeof out, "%05d|%-4s|%x", 42, "ab", 255);
    printf("wtest: %s\n", out);
    int a, b;
    if (sscanf("12,34", "%d,%d", &a, &b) == 2) printf("wtest: sscanf %d\n", a + b);

    HANDLE h = CreateFileA("wtest2.txt", GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL);
    DWORD k;
    WriteFile(h, "win32 file", 10, &k, NULL);
    CloseHandle(h);
    h = CreateFileA("wtest2.txt", GENERIC_READ, 0, NULL, OPEN_EXISTING, 0, NULL);
    char rb[32] = {0};
    ReadFile(h, rb, 31, &k, NULL);
    printf("wtest: win32 read %lu '%s' size %lu\n", k, rb, GetFileSize(h, NULL));
    CloseHandle(h);
    if (CreateFileA("missing.txt", GENERIC_READ, 0, NULL, OPEN_EXISTING, 0, NULL) == INVALID_HANDLE_VALUE)
        printf("wtest: missing file error %lu\n", GetLastError());

    printf("wtest: type a number: ");
    fflush(stdout);
    int x;
    if (scanf("%d", &x) == 1) printf("wtest: you typed %d, doubled %d\n", x, 2 * x);
    return 0;
}
