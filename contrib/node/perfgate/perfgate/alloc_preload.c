// Heap allocation counter for node workloads (PF0), loaded with LD_PRELOAD.
//
// Interposes the C allocator entry points (Rust's System allocator and C++ operator new both end
// here), forwards to the next definition, and at exit appends one JSON line with the counts, the
// process's /proc/self/io totals and its peak RSS to "$PERFGATE_ALLOC_OUT.<pid>", or (PF0-N jails, which have
// nowhere to write files the gate trusts) as one line with its pid to the pipe "$PERFGATE_ALLOC_FD", which the
// broker validates (one record per pid, a line well under PIPE_BUF so it is written atomically). Works the same for
// dashd and a Rust node using the system allocator; a node with its own allocator shows 0 allocations.
#define _GNU_SOURCE
#include <dlfcn.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

static void *(*real_malloc)(size_t);
static void *(*real_calloc)(size_t, size_t);
static void *(*real_realloc)(void *, size_t);
static void (*real_free)(void *);
static int (*real_posix_memalign)(void **, size_t, size_t);
static void *(*real_aligned_alloc)(size_t, size_t);
static void *(*real_memalign)(size_t, size_t);

static _Atomic uint64_t n_allocs, n_bytes;

// dlsym may allocate before the real functions are known; serve that from a static arena.
static _Alignas(16) char boot[8192];
static size_t boot_used;
static int resolving;

static int in_boot(void *p) { return (char *)p >= boot && (char *)p < boot + sizeof boot; }

static void *boot_alloc(size_t n) {
    n = (n + 15) & ~(size_t)15;
    if (boot_used + n > sizeof boot) return NULL;
    void *p = boot + boot_used;
    boot_used += n;
    return p;
}

// A forked child starts from the parent's counts; zero them so its exit line only has its own.
static void atfork_child(void) {
    atomic_store(&n_allocs, 0);
    atomic_store(&n_bytes, 0);
}

// The alloc pipe as the process started with it: the exit line goes there only if fd PERFGATE_ALLOC_FD is still
// that pipe (a node that closed it and reused the number for a file must not get the line in its data).
static dev_t pipe_dev;
static ino_t pipe_ino;
static int pipe_ok;

__attribute__((constructor)) static void init(void) {
    pthread_atfork(NULL, NULL, atfork_child);
    const char *pipe_fd = getenv("PERFGATE_ALLOC_FD");
    struct stat st;
    if (pipe_fd && fstat(atoi(pipe_fd), &st) == 0 && S_ISFIFO(st.st_mode)) {
        pipe_dev = st.st_dev;
        pipe_ino = st.st_ino;
        pipe_ok = 1;
    }
}

static void resolve(void) {
    if (real_malloc || resolving) return;
    resolving = 1;
    real_calloc = dlsym(RTLD_NEXT, "calloc");
    real_realloc = dlsym(RTLD_NEXT, "realloc");
    real_free = dlsym(RTLD_NEXT, "free");
    real_posix_memalign = dlsym(RTLD_NEXT, "posix_memalign");
    real_aligned_alloc = dlsym(RTLD_NEXT, "aligned_alloc");
    real_memalign = dlsym(RTLD_NEXT, "memalign");
    real_malloc = dlsym(RTLD_NEXT, "malloc");
    resolving = 0;
}

static void count(size_t n) {
    atomic_fetch_add_explicit(&n_allocs, 1, memory_order_relaxed);
    atomic_fetch_add_explicit(&n_bytes, n, memory_order_relaxed);
}

void *malloc(size_t n) {
    if (!real_malloc) {
        if (resolving) return boot_alloc(n);
        resolve();
    }
    count(n);
    return real_malloc(n);
}

void *calloc(size_t a, size_t b) {
    if (!real_calloc) {
        if (resolving) return boot_alloc(a * b);  // the arena is zero-initialised and never reused
        resolve();
    }
    count(a * b);
    return real_calloc(a, b);
}

void *realloc(void *p, size_t n) {
    resolve();
    if (!real_realloc || !real_malloc) {  // only while dlsym is running: stay in the arena
        if (p && !in_boot(p)) return NULL;
        void *q = boot_alloc(n);
        if (q && p) memcpy(q, p, n < (size_t)(boot + sizeof boot - (char *)p) ? n : (size_t)(boot + sizeof boot - (char *)p));
        return q;
    }
    count(n);
    if (in_boot(p)) {
        // The old size is unknown: copy up to the arena's end (the extra bytes are never read).
        size_t avail = (size_t)(boot + sizeof boot - (char *)p);
        void *q = real_malloc(n);
        if (q) memcpy(q, p, n < avail ? n : avail);
        return q;
    }
    return real_realloc(p, n);
}

void free(void *p) {
    if (!p || in_boot(p)) return;
    resolve();
    real_free(p);
}

int posix_memalign(void **out, size_t align, size_t n) {
    resolve();
    if (!real_posix_memalign) return (*out = boot_alloc(n)) ? 0 : 12;  // ENOMEM
    count(n);
    return real_posix_memalign(out, align, n);
}

void *aligned_alloc(size_t align, size_t n) {
    resolve();
    if (!real_aligned_alloc) return boot_alloc(n);
    count(n);
    return real_aligned_alloc(align, n);
}

void *memalign(size_t align, size_t n) {
    resolve();
    if (!real_memalign) return boot_alloc(n);
    count(n);
    return real_memalign(align, n);
}

static uint64_t field(const char *text, const char *key) {
    const char *p = strstr(text, key);
    return p ? strtoull(p + strlen(key), NULL, 10) : 0;
}

static size_t slurp(const char *path, char *buf, size_t cap) {
    int fd = open(path, O_RDONLY | O_CLOEXEC);
    if (fd < 0) return 0;
    size_t len = 0;
    ssize_t n;
    while (len + 1 < cap && (n = read(fd, buf + len, cap - 1 - len)) > 0) len += (size_t)n;
    close(fd);
    buf[len] = 0;
    return len;
}

__attribute__((destructor)) static void report(void) {
    const char *base = getenv("PERFGATE_ALLOC_OUT");
    const char *pipe_fd = getenv("PERFGATE_ALLOC_FD");
    if (!base && !pipe_fd) return;
    char io[1024], status[8192], path[4096], line[1024];
    slurp("/proc/self/io", io, sizeof io);
    slurp("/proc/self/status", status, sizeof status);
    int len = snprintf(line, sizeof line, pipe_fd ? "{\"pid\":%d," : "{", (int)getpid());
    len += snprintf(line + len, sizeof line - (size_t)len,
                       "\"allocs\":%llu,\"alloc_bytes\":%llu,\"read_bytes\":%llu,\"write_bytes\":%llu,"
                       "\"read_syscalls\":%llu,\"write_syscalls\":%llu,\"peak_rss_kib\":%llu}\n",
                       (unsigned long long)atomic_load(&n_allocs), (unsigned long long)atomic_load(&n_bytes),
                       (unsigned long long)field(io, "rchar:"), (unsigned long long)field(io, "wchar:"),
                       (unsigned long long)field(io, "syscr:"), (unsigned long long)field(io, "syscw:"),
                       (unsigned long long)field(status, "VmHWM:"));
    if (pipe_fd) {
        struct stat st;
        int fd = atoi(pipe_fd);
        if (pipe_ok && fstat(fd, &st) == 0 && S_ISFIFO(st.st_mode) && st.st_dev == pipe_dev && st.st_ino == pipe_ino) {
            ssize_t w = write(fd, line, (size_t)len);
            (void)w;
        }
        return;
    }
    snprintf(path, sizeof path, "%s.%d", base, (int)getpid());
    int fd = open(path, O_WRONLY | O_CREAT | O_APPEND | O_CLOEXEC, 0644);
    if (fd >= 0) {
        ssize_t w = write(fd, line, (size_t)len);
        (void)w;
        close(fd);
    }
}
