// The marker PR binary of the malicious-node-bench test (plan rev4.1 §11, D6). On every start it sends one UDP
// datagram to 127.0.0.1:MARKER_PORT in the invocation's netns with its cgroup, pid namespace and exe, so the test
// can prove every execution happened inside a spawn jail. Then: `-version` prints and exits 0; otherwise it
// touches <datadir>/ready and exits 0 on SIGTERM. MARKER (a string) and MARKER_PORT are set at compile time.
#define _GNU_SOURCE
#include <arpa/inet.h>
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>

#ifndef MARKER
#define MARKER "pf0-marker"
#endif
#ifndef MARKER_PORT
#define MARKER_PORT 19599
#endif

static const char marker[] = MARKER;

static void first_line(const char *path, char *out, size_t n) {
    int fd = open(path, O_RDONLY);
    ssize_t k = fd < 0 ? 0 : read(fd, out, n - 1);
    out[k < 0 ? 0 : k] = 0;
    if (fd >= 0) close(fd);
    char *nl = strchr(out, '\n');
    if (nl) *nl = 0;
}

static void link_of(const char *path, char *out, size_t n) {
    ssize_t k = readlink(path, out, n - 1);
    out[k < 0 ? 0 : k] = 0;
}

int main(int argc, char **argv) {
    char cg[512], ns[128], exe[512], msg[1400];
    first_line("/proc/self/cgroup", cg, sizeof cg);
    link_of("/proc/self/ns/pid", ns, sizeof ns);
    link_of("/proc/self/exe", exe, sizeof exe);
    int n = snprintf(msg, sizeof msg, "{\"marker\":\"%s\",\"cgroup\":\"%s\",\"pidns\":\"%s\",\"exe\":\"%s\"}", marker,
                     cg, ns, exe);
    int s = socket(AF_INET, SOCK_DGRAM, 0);
    struct sockaddr_in a = {.sin_family = AF_INET, .sin_port = htons(MARKER_PORT)};
    a.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    if (s >= 0) sendto(s, msg, (size_t)n, 0, (struct sockaddr *)&a, sizeof a);
    const char *datadir = NULL;
    int version = 0;
    for (int i = 1; i < argc; i++) {
        if (!strncmp(argv[i], "-datadir=", 9)) datadir = argv[i] + 9;
        if (!strcmp(argv[i], "-version")) version = 1;
    }
    if (version) {
        printf("marker rdashd %s\n", marker);
        return 0;
    }
    sigset_t set;
    sigemptyset(&set);
    sigaddset(&set, SIGTERM);
    sigprocmask(SIG_BLOCK, &set, NULL);
    if (datadir) {
        char p[600];
        snprintf(p, sizeof p, "%s/ready", datadir);
        int fd = open(p, O_WRONLY | O_CREAT, 0600);
        if (fd >= 0) close(fd);
    }
    int sig;
    sigwait(&set, &sig);
    return 0;
}
