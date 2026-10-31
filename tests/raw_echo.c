/* Hardware fixture for unmodified raw clients and receive-only streams. */
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

int main(int argc, char **argv)
{
    if (argc < 2 || argc > 3) return 2;
    int receive_only = argc == 3 && strcmp(argv[2], "receive-only") == 0;
    int fd = open(argv[1], O_RDWR);
    if (fd < 0) { perror("open target"); return 1; }
    unsigned char buffer[8197];
    memcpy(buffer, "ACK: ", 5);
    for (unsigned n = 0; n < 2; n++) {
        ssize_t got;
        do { got = read(fd, buffer + 5, 8192); } while (got < 0 && errno == EINTR);
        if (got <= 0) { perror("read target"); close(fd); return 1; }
        printf("received=%zd first=%02x last=%02x\n", got, buffer[5], buffer[4 + got]);
        fflush(stdout);
        if (!receive_only) {
            size_t size = 5 + (got > 8187 ? 8187 : (size_t)got);
            if (write(fd, buffer, size) != (ssize_t)size) {
                perror("write target"); close(fd); return 1;
            }
        }
    }
    if (!receive_only) {
        /* Retain the descriptor until the harness finishes both response reads. */
        for (;;) pause();
    }
    close(fd);
    return 0;
}
