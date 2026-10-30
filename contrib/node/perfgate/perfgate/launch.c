// The trusted step between a counter (perf, strace) and the sandbox (PF0-N, BL-128).
//
//     launch --exec -- bwrap ... -- <code under test>
//
// `perf stat -o FILE` keeps FILE open in the process it starts, and everything below inherits that
// descriptor. bwrap passes it on, so the code under test can reopen FILE through /proc/self/fd/N and
// rewrite the count the gate reads (F0), although the sandbox never mounts FILE. This makes stdin
// /dev/null (a counter started with fd 0 closed opens FILE as fd 0), closes every descriptor above stderr
// and execs the rest of the command line. stdout and stderr stay as the caller made them: pipes, or files
// the code under test may write anyway. It is compiled by the gate (no PR code) and adds a fixed ~126k
// user-space instructions to `instructions_process`.
#define _GNU_SOURCE
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

int main(int argc, char **argv) {
    if (argc < 4 || strcmp(argv[1], "--exec") != 0 || strcmp(argv[2], "--") != 0) {
        fprintf(stderr, "usage: launch --exec -- COMMAND [ARG...]\n");
        return 125;
    }
    int null = open("/dev/null", O_RDONLY);
    if (null < 0 || (null != 0 && (dup2(null, 0) != 0 || close(null) != 0))) {
        perror("launch: stdin");
        return 125;
    }
    if (close_range(3, ~0U, 0) != 0) {
        perror("launch: close_range");
        return 125;
    }
    execvp(argv[3], argv + 3);
    perror("launch: exec");
    return 127;
}
