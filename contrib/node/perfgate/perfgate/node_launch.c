// The trusted C steps of a PF0-N node spawn (BL-128; plan rev4.1 D24-D27, D37, D44, D68).
//
//     node-launch --into SOCK --cpus LIST --node N -- CMD...   placement helper (broker's child, in <scope>/harness)
//     node-launch --jail SOCK --info FD --alloc FD --bwrap BWRAP  last step before the jail (under perf when measured)
//     node-launch --client --class CLASS --measure M -- ARGV0 ARGS...
//                                                               the stub node-bench runs as its node (in the view)
//
// Every personality makes itself non-dumpable before anything else, so a same-uid process that can see it
// cannot read its memory, open its descriptors or ptrace it, and it only receives protected material (the
// spawn's cgroup, the jail spec) over SOCK after it said hello with `dumpable:0`. Dumpability resets at exec,
// so nothing protected is inherited across one, except the sealed spec memfd that bwrap must read (--args).
//
// --into: clone3(CLONE_INTO_CGROUP|CLONE_PIDFD) puts the child into spawn-k before its first instruction; the
// child pins itself (lowest CPUs of NUMA node N, MPOL_BIND N, ADDR_NO_RANDOMIZE), reads all of it back and execs
// CMD; the helper reports the child's pid, then its wait status and rusage, on SOCK. No fallback: clone3 failing
// is exit 124. bwrap's exec resets the personality and clears PDEATHSIG (its AppArmor profile change), so the
// jail runs `setarch -R`, and the broker's cgroup.kill backs PDEATHSIG for an unmeasured spawn.
//
// --jail: receives the sealed spec (bwrap's options), the jail's command and the datadir's descriptor over SOCK,
// moves the alloc pipe to fd 5, the info pipe to fd 6, the spec to fd 7 and the datadir to fd 8 (bwrap's
// --bind-fd 8), makes stdin /dev/null, closes everything else and execs `bwrap --args 7 -- COMMAND` with an
// empty environment.
//
// --client: sends {argv, cwd, measure, class} with its stdin, stdout and stderr to the broker's socket, then
// mirrors the spawn: SIGTERM becomes {stop}, and it exits with the spawn's code or raises its signal.
//
// A test build (-DPF0_TEST_HOOKS, never the gate's) takes `--test-hooks LIST` first: stop-before-exec (the clone3
// child stops before exec), hold (stop once the protected material arrived), entry-hold (stop at entry, before
// the prctl, so its inherited descriptors can be listed), comm=TOKEN (PR_SET_NAME), dumpable (skip the prctl:
// the non-dumpability control), clone3-einval (clone3 fails as on a host without it) and hang-after-child (the
// helper never waits for its child).
//
// It is compiled by the gate from its own sources (no PR code). The bench launcher (launch.c) is separate and
// unchanged: none of this is in a bench row's counted chain (D44).
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <linux/sched.h>
#include <poll.h>
#include <sched.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/personality.h>
#include <sys/prctl.h>
#include <sys/resource.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/un.h>
#include <sys/wait.h>
#include <unistd.h>

#define MPOL_BIND_ 2
#define MSG_MAX 65536
#define BROKER_SOCKET "/pf0/ctl/broker"

static const char *who = "node-launch";

#ifdef PF0_TEST_HOOKS
static struct {
    int stop_before_exec, hold, entry_hold, dumpable, clone3_einval, hang_after_child;
    const char *comm;
} hooks;
#define HOLD()                    \
    do {                          \
        if (hooks.hold) raise(SIGSTOP); \
    } while (0)
#else
#define HOLD() \
    do {       \
    } while (0)
#endif

static void die(int code, const char *what) {
    fprintf(stderr, "%s: %s: %s\n", who, what, errno ? strerror(errno) : "failed");
    _exit(code);
}

static void undumpable(void) {
#ifdef PF0_TEST_HOOKS
    if (hooks.dumpable) return;
#endif
    if (prctl(PR_SET_DUMPABLE, 0, 0, 0, 0) != 0 || prctl(PR_GET_DUMPABLE, 0, 0, 0, 0) != 0) die(125, "PR_SET_DUMPABLE");
}

static int parse_fd(const char *s) {
    char *end;
    long v = strtol(s, &end, 10);
    if (*s == 0 || *end != 0 || v < 0 || v > 1 << 20) {
        errno = EINVAL;
        die(125, "bad descriptor number");
    }
    return (int)v;
}

static void send_text(int sock, const char *text) {
    if (send(sock, text, strlen(text), MSG_NOSIGNAL) < 0) die(125, "send");
}

static void hello(int sock, const char *personality) {
    char buf[128];
    snprintf(buf, sizeof buf, "{\"hello\":\"%s\",\"dumpable\":%d}", personality, prctl(PR_GET_DUMPABLE, 0, 0, 0, 0));
    send_text(sock, buf);
}

// One message with exactly `n` descriptors into `out` (the payload must be `expect`).
static void recv_fds(int sock, const char *expect, int *out, int count) {
    char buf[256];
    char ctl[CMSG_SPACE(sizeof(int) * 4)];
    struct iovec iov = {buf, sizeof buf - 1};
    struct msghdr m = {.msg_iov = &iov, .msg_iovlen = 1, .msg_control = ctl, .msg_controllen = sizeof ctl};
    ssize_t n = recvmsg(sock, &m, MSG_CMSG_CLOEXEC);
    if (n <= 0) die(125, "handshake: no reply");
    buf[n] = 0;
    struct cmsghdr *c = CMSG_FIRSTHDR(&m);
    if (strcmp(buf, expect) != 0 || !c || c->cmsg_level != SOL_SOCKET || c->cmsg_type != SCM_RIGHTS ||
        c->cmsg_len != CMSG_LEN(sizeof(int) * (size_t)count) || (m.msg_flags & (MSG_CTRUNC | MSG_TRUNC))) {
        errno = EPROTO;
        die(125, "handshake: unexpected reply");
    }
    memcpy(out, CMSG_DATA(c), sizeof(int) * (size_t)count);
}

static int recv_fd(int sock, const char *expect) {
    int fd;
    recv_fds(sock, expect, &fd, 1);
    return fd;
}

static void sealed(int fd) {
    int seals = fcntl(fd, F_GET_SEALS);
    int need = F_SEAL_WRITE | F_SEAL_GROW | F_SEAL_SHRINK | F_SEAL_SEAL;
    if (seals < 0 || (seals & need) != need) {
        errno = EPERM;
        die(125, "the spec is not sealed");
    }
}

// "0,1,5" -> mask; returns the count.
static int parse_cpus(const char *s, cpu_set_t *set) {
    CPU_ZERO(set);
    int n = 0;
    char *copy = strdup(s), *save = NULL;
    for (char *t = strtok_r(copy, ",", &save); t; t = strtok_r(NULL, ",", &save)) {
        char *end;
        long c = strtol(t, &end, 10);
        if (*t == 0 || *end != 0 || c < 0 || c >= CPU_SETSIZE) {
            errno = EINVAL;
            die(125, "bad --cpus");
        }
        CPU_SET((int)c, set);
        n++;
    }
    free(copy);
    return n;
}

static void place(const char *cpus, int node) {
    cpu_set_t want, got;
    if (parse_cpus(cpus, &want) == 0) die(125, "empty --cpus");
    if (sched_setaffinity(0, sizeof want, &want) != 0) {
        fprintf(stderr, "%s: placement unavailable: CPUs %s: %s\n", who, cpus, strerror(errno));
        _exit(125);
    }
    unsigned long mask[16] = {0};
    if (node < 0 || node >= (int)(sizeof mask * 8)) die(125, "bad --node");
    mask[node / (8 * sizeof(unsigned long))] |= 1UL << (node % (8 * sizeof(unsigned long)));
    if (syscall(SYS_set_mempolicy, MPOL_BIND_, mask, sizeof mask * 8 + 1) != 0) {
        fprintf(stderr, "%s: placement unavailable: NUMA node %d: %s\n", who, node, strerror(errno));
        _exit(125);
    }
    if (personality(personality(0xffffffff) | ADDR_NO_RANDOMIZE) == -1) die(125, "personality");
    // Read everything back: a cpuset or policy that silently narrowed or ignored a request is an error.
    int mode = -1;
    unsigned long back[16] = {0};
    if (sched_getaffinity(0, sizeof got, &got) != 0 || !CPU_EQUAL(&got, &want) ||
        syscall(SYS_get_mempolicy, &mode, back, sizeof back * 8 + 1, NULL, 0UL) != 0 || mode != MPOL_BIND_ ||
        memcmp(back, mask, sizeof mask) != 0 || !(personality(0xffffffff) & ADDR_NO_RANDOMIZE)) {
        fprintf(stderr, "%s: placement unavailable: CPUs %s / node %d not in effect\n", who, cpus, node);
        _exit(125);
    }
}

static int into(int argc, char **argv) {
    who = "node-launch --into";
    // node-launch --into SOCK --cpus LIST --node N -- CMD...
    if (argc < 9 || strcmp(argv[3], "--cpus") || strcmp(argv[5], "--node") || strcmp(argv[7], "--")) {
        fprintf(stderr, "usage: node-launch --into SOCK --cpus LIST --node N -- CMD...\n");
        return 125;
    }
    int sock = parse_fd(argv[2]);
    const char *cpus = argv[4];
    int node = (int)parse_fd(argv[6]);
    char **cmd = argv + 8;
    hello(sock, "into");
    int cg = recv_fd(sock, "{\"cgroup\":1}");
    HOLD();
    int pidfd = -1;
    struct clone_args ca;
    memset(&ca, 0, sizeof ca);
    ca.flags = CLONE_INTO_CGROUP | CLONE_PIDFD;
    ca.pidfd = (uint64_t)(uintptr_t)&pidfd;
    ca.exit_signal = SIGCHLD;
    ca.cgroup = (uint64_t)cg;
    pid_t parent = getpid();
    long pid = syscall(SYS_clone3, &ca, sizeof ca);
#ifdef PF0_TEST_HOOKS
    if (hooks.clone3_einval && pid > 0) {
        kill((pid_t)pid, SIGKILL);
        waitpid((pid_t)pid, NULL, 0);
        pid = -1;
        errno = EINVAL;
    } else if (hooks.clone3_einval && pid == 0) {
        pause();
    }
#endif
    if (pid < 0) {
        char msg[256];
        snprintf(msg, sizeof msg, "{\"error\":\"atomic cgroup placement unavailable: clone3: %s\"}", strerror(errno));
        send_text(sock, msg);
        fprintf(stderr, "%s: atomic cgroup placement unavailable: clone3: %s\n", who, strerror(errno));
        return 124;
    }
    if (pid == 0) {
        who = "node-launch --into (child)";
        if (prctl(PR_SET_PDEATHSIG, SIGKILL, 0, 0, 0) != 0 || getppid() != parent) _exit(125);
        struct rlimit nocore = {0, 0};
        if (setpgid(0, 0) != 0 || setrlimit(RLIMIT_CORE, &nocore) != 0) die(125, "setpgid/RLIMIT_CORE");
        close(cg);
        close(sock);
        place(cpus, node);
#ifdef PF0_TEST_HOOKS
        if (hooks.stop_before_exec) raise(SIGSTOP);
#endif
        execvp(cmd[0], cmd);
        die(127, "exec");
    }
    close(cg);
    // Only the child keeps the jail's socket and the info and alloc pipes: their ends must close with the chain.
    if ((sock > 3 && close_range(3, (unsigned)sock - 1, 0) != 0) || close_range((unsigned)sock + 1, ~0U, 0) != 0)
        die(125, "close_range");
    char msg[256];
    snprintf(msg, sizeof msg, "{\"child\":%ld}", pid);
    send_text(sock, msg);
#ifdef PF0_TEST_HOOKS
    if (hooks.hang_after_child)
        for (;;) pause();
#endif
    int status;
    struct rusage ru;
    pid_t w;
    while ((w = wait4((pid_t)pid, &status, 0, &ru)) < 0 && errno == EINTR) {
    }
    if (w < 0) die(125, "wait4");
    snprintf(msg, sizeof msg,
             "{\"%s\":%d,\"rusage\":{\"utime_us\":%lld,\"stime_us\":%lld,\"maxrss_kib\":%ld,\"minflt\":%ld,"
             "\"majflt\":%ld}}",
             WIFEXITED(status) ? "exit" : "signal", WIFEXITED(status) ? WEXITSTATUS(status) : WTERMSIG(status),
             (long long)ru.ru_utime.tv_sec * 1000000 + ru.ru_utime.tv_usec,
             (long long)ru.ru_stime.tv_sec * 1000000 + ru.ru_stime.tv_usec, ru.ru_maxrss, ru.ru_minflt, ru.ru_majflt);
    send_text(sock, msg);
    return 0;
}

static int jail(int argc, char **argv) {
    who = "node-launch --jail";
    // node-launch --jail SOCK --info FD --alloc FD --bwrap PATH
    if (argc != 9 || strcmp(argv[3], "--info") || strcmp(argv[5], "--alloc") || strcmp(argv[7], "--bwrap")) {
        fprintf(stderr, "usage: node-launch --jail SOCK --info FD --alloc FD --bwrap PATH\n");
        return 125;
    }
    int sock = parse_fd(argv[2]), info = parse_fd(argv[4]), alloc = parse_fd(argv[6]);
    const char *bwrap = argv[8];
    hello(sock, "jail");
    int got[3];
    recv_fds(sock, "{\"spec\":3}", got, 3);
    int spec = got[0], cmdfd = got[1], datadir = got[2];
    sealed(spec);
    sealed(cmdfd);
    struct stat dst;
    if (fstat(datadir, &dst) != 0 || !S_ISDIR(dst.st_mode)) {
        errno = ENOTDIR;
        die(125, "the datadir descriptor");
    }
    HOLD();
    close(sock);
    // bwrap takes only options from --args: the jail's command (NUL-separated) comes in the second memfd.
    static char cmdbuf[MSG_MAX];
    static char *bargv[1024];
    ssize_t len = pread(cmdfd, cmdbuf, sizeof cmdbuf, 0);
    if (len <= 0 || len == (ssize_t)sizeof cmdbuf || cmdbuf[len - 1] != 0) {
        errno = E2BIG;
        die(125, "the jail command");
    }
    close(cmdfd);
    int na = 0;
    bargv[na++] = "bwrap";
    bargv[na++] = "--args";
    bargv[na++] = "7";
    bargv[na++] = "--";
    for (ssize_t i = 0; i < len; i += (ssize_t)strlen(cmdbuf + i) + 1) {
        if (na == 1023) {
            errno = E2BIG;
            die(125, "the jail command");
        }
        bargv[na++] = cmdbuf + i;
    }
    bargv[na] = NULL;
    // Move the three descriptors bwrap needs to fixed numbers above stdio, out of the way first.
    int hi_info = fcntl(info, F_DUPFD, 100), hi_alloc = fcntl(alloc, F_DUPFD, 100), hi_spec = fcntl(spec, F_DUPFD, 100),
        hi_dir = fcntl(datadir, F_DUPFD, 100);
    if (hi_info < 0 || hi_alloc < 0 || hi_spec < 0 || hi_dir < 0 || dup2(hi_alloc, 5) != 5 || dup2(hi_info, 6) != 6 ||
        dup2(hi_spec, 7) != 7 || dup2(hi_dir, 8) != 8)
        die(125, "descriptors");
    int null = open("/dev/null", O_RDONLY);
    if (null < 0 || dup2(null, 0) != 0) die(125, "stdin");
    if (close_range(3, 4, 0) != 0 || close_range(9, ~0U, 0) != 0) die(125, "close_range");
    char *envp[] = {NULL};
    execve(bwrap, bargv, envp);
    die(127, "exec bwrap");
    return 127;
}

// A JSON string: quotes, backslashes and control bytes escaped, every other byte as is (UTF-8 passes through;
// the broker decodes strictly, so invalid UTF-8, encoded surrogates included, is refused). Dies rather than send a
// truncated string.
static size_t put_json_string(char *out, size_t cap, size_t len, const char *s) {
    if (len + 1 >= cap) goto full;
    out[len++] = '"';
    for (const unsigned char *p = (const unsigned char *)s; *p; p++) {
        if (len + 8 >= cap) goto full;
        if (*p == '"' || *p == '\\') {
            out[len++] = '\\';
            out[len++] = (char)*p;
        } else if (*p < 0x20 || *p == 0x7f) {
            len += (size_t)snprintf(out + len, cap - len, "\\u%04x", *p);
        } else {
            out[len++] = (char)*p;
        }
    }
    if (len + 1 >= cap) goto full;
    out[len++] = '"';
    return len;
full:
    errno = E2BIG;
    die(125, "request too large");
    return 0;
}

static volatile sig_atomic_t term_pending;
static int self_pipe[2];

static void on_term(int sig) {
    (void)sig;
    term_pending = 1;
    int saved = errno;
    ssize_t w = write(self_pipe[1], "t", 1);
    (void)w;
    errno = saved;
}

static int client(int argc, char **argv) {
    who = "node-launch --client";
    // node-launch --client --class CLASS --measure M -- ARGV0 ARGS...
    if (argc < 8 || strcmp(argv[2], "--class") || strcmp(argv[4], "--measure") || strcmp(argv[6], "--")) {
        fprintf(stderr, "usage: node-launch --client --class CLASS --measure M -- ARGV0 ARGS...\n");
        return 125;
    }
    static char req[MSG_MAX];
    size_t len = (size_t)snprintf(req, sizeof req, "{\"class\":");
    len = put_json_string(req, sizeof req, len, argv[3]);
    len += (size_t)snprintf(req + len, sizeof req - len, ",\"measure\":");
    len = put_json_string(req, sizeof req, len, argv[5]);
    char cwd[4096];
    if (!getcwd(cwd, sizeof cwd)) die(125, "getcwd");
    len += (size_t)snprintf(req + len, sizeof req - len, ",\"cwd\":");
    len = put_json_string(req, sizeof req, len, cwd);
    len += (size_t)snprintf(req + len, sizeof req - len, ",\"argv\":[");
    for (int i = 8; i < argc; i++) {  // argv[7] is the node's own name (this stub): not sent
        if (i > 8) req[len++] = ',';  // put_json_string left room for it
        len = put_json_string(req, sizeof req, len, argv[i]);
    }
    if (len + 3 >= sizeof req) {
        errno = E2BIG;
        die(125, "request too large");
    }
    req[len++] = ']';
    req[len++] = '}';
    // Signals first: a SIGTERM from here on becomes {stop} (graceful), never the default death (an EOF kill).
    if (pipe2(self_pipe, O_CLOEXEC | O_NONBLOCK) != 0) die(125, "pipe");
    struct sigaction sa = {.sa_handler = on_term};
    sigemptyset(&sa.sa_mask);
    sigaction(SIGTERM, &sa, NULL);
    sigaction(SIGINT, &sa, NULL);
    int s = socket(AF_UNIX, SOCK_SEQPACKET | SOCK_CLOEXEC, 0);
    struct sockaddr_un a = {.sun_family = AF_UNIX};
    const char *path = getenv("PF0_BROKER_SOCKET") ? getenv("PF0_BROKER_SOCKET") : BROKER_SOCKET;
    snprintf(a.sun_path, sizeof a.sun_path, "%s", path);
    if (s < 0 || connect(s, (struct sockaddr *)&a, sizeof a) != 0) die(125, "connect to the broker");
    int fds[3] = {0, 1, 2};
    char ctl[CMSG_SPACE(sizeof fds)];
    struct iovec iov = {req, len};
    struct msghdr m = {.msg_iov = &iov, .msg_iovlen = 1, .msg_control = ctl, .msg_controllen = sizeof ctl};
    struct cmsghdr *c = CMSG_FIRSTHDR(&m);
    c->cmsg_level = SOL_SOCKET;
    c->cmsg_type = SCM_RIGHTS;
    c->cmsg_len = CMSG_LEN(sizeof fds);
    memcpy(CMSG_DATA(c), fds, sizeof fds);
    if (sendmsg(s, &m, MSG_NOSIGNAL) < 0) die(125, "send the request");
    HOLD();
    int stop_sent = 0;
    for (;;) {
        if (term_pending && !stop_sent) {
            send_text(s, "{\"stop\":true}");
            stop_sent = 1;
        }
        struct pollfd p[2] = {{s, POLLIN, 0}, {self_pipe[0], POLLIN, 0}};
        if (poll(p, 2, -1) < 0) {
            if (errno == EINTR) continue;
            die(125, "poll");
        }
        if (p[1].revents) {
            char drain[16];
            while (read(self_pipe[0], drain, sizeof drain) > 0) {
            }
            continue;
        }
        if (!p[0].revents) continue;
        char buf[4096];
        ssize_t n = recv(s, buf, sizeof buf - 1, 0);
        if (n <= 0) {
            fprintf(stderr, "%s: the broker closed the connection\n", who);
            return 125;
        }
        buf[n] = 0;
        int v;
        if (sscanf(buf, "{\"spawn\":%d}", &v) == 1) continue;
        if (sscanf(buf, "{\"exit\":%d}", &v) == 1) return v;
        if (sscanf(buf, "{\"signal\":%d}", &v) == 1) {
            signal(v, SIG_DFL);
            raise(v);
            return 128 + v;
        }
        fprintf(stderr, "%s: broker: %s\n", who, buf);
        return 125;
    }
}

#ifdef PF0_TEST_HOOKS
static void parse_hooks(char *list) {
    char *save = NULL;
    for (char *t = strtok_r(list, ",", &save); t; t = strtok_r(NULL, ",", &save)) {
        if (!strcmp(t, "stop-before-exec")) hooks.stop_before_exec = 1;
        else if (!strcmp(t, "hold")) hooks.hold = 1;
        else if (!strcmp(t, "entry-hold")) hooks.entry_hold = 1;
        else if (!strcmp(t, "hang-after-child")) hooks.hang_after_child = 1;
        else if (!strcmp(t, "dumpable")) hooks.dumpable = 1;
        else if (!strcmp(t, "clone3-einval")) hooks.clone3_einval = 1;
        else if (!strncmp(t, "comm=", 5)) hooks.comm = t + 5;
        else {
            errno = EINVAL;
            die(125, "bad --test-hooks");
        }
    }
}
#endif

int main(int argc, char **argv) {
#ifdef PF0_TEST_HOOKS
    if (argc >= 3 && strcmp(argv[1], "--test-hooks") == 0) {
        parse_hooks(argv[2]);
        argv[2] = argv[0];
        argv += 2;
        argc -= 2;
        if (hooks.entry_hold) raise(SIGSTOP);
    }
#endif
    undumpable();
#ifdef PF0_TEST_HOOKS
    if (hooks.comm && prctl(PR_SET_NAME, hooks.comm, 0, 0, 0) != 0) die(125, "PR_SET_NAME");
#endif
    if (argc >= 2 && strcmp(argv[1], "--into") == 0 && argc >= 3) return into(argc, argv);
    if (argc >= 2 && strcmp(argv[1], "--jail") == 0 && argc >= 3) return jail(argc, argv);
    if (argc >= 2 && strcmp(argv[1], "--client") == 0) return client(argc, argv);
    fprintf(stderr, "usage: node-launch --into|--jail|--client ...\n");
    return 125;
}
