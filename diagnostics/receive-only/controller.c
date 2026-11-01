// SPDX-License-Identifier: MIT
#define _POSIX_C_SOURCE 200809L
#include <errno.h>
#include <fcntl.h>
#include <getopt.h>
#include <linux/gpio.h>
#include <linux/i2c-dev.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/file.h>
#include <sys/ioctl.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

static volatile sig_atomic_t stopped;
static void finish(int sig) { (void)sig; stopped = 1; }
static uint64_t now_ns(void)
{
    struct timespec t;
    if (clock_gettime(CLOCK_MONOTONIC, &t)) { perror("clock_gettime"); exit(2); }
    return (uint64_t)t.tv_sec * 1000000000ULL + t.tv_nsec;
}
static void delay_us(uint64_t n)
{
    struct timespec t = { .tv_sec = n / 1000000, .tv_nsec = (n % 1000000) * 1000 };
    while (!stopped && nanosleep(&t, &t) && errno == EINTR) {}
}
static int marker_request(const char *chip, unsigned line)
{
    int fd = open(chip, O_RDONLY);
    if (fd < 0) { perror("open gpiochip"); exit(2); }
    struct gpio_v2_line_request r = {0};
    r.offsets[0] = line; r.num_lines = 1;
    strcpy(r.consumer, "rx-only-first-error");
    r.config.flags = GPIO_V2_LINE_FLAG_OUTPUT | GPIO_V2_LINE_FLAG_OPEN_DRAIN |
                     GPIO_V2_LINE_FLAG_BIAS_PULL_UP;
    r.config.num_attrs = 1;
    r.config.attrs[0].attr.id = GPIO_V2_LINE_ATTR_ID_OUTPUT_VALUES;
    r.config.attrs[0].attr.values = 1; /* Released, never counter-drive a target. */
    r.config.attrs[0].mask = 1;
    if (ioctl(fd, GPIO_V2_GET_LINE_IOCTL, &r)) { perror("request marker"); exit(2); }
    close(fd);
    struct gpio_v2_line_values v = { .mask = 1 };
    if (ioctl(r.fd, GPIO_V2_LINE_GET_VALUES_IOCTL, &v) || !(v.bits & 1)) {
        fprintf(stderr, "marker did not release high; no bus traffic permitted\n"); exit(2);
    }
    return r.fd;
}
static int mark_error(int marker)
{
    struct gpio_v2_line_values v = { .bits = 0, .mask = 1 };
    return ioctl(marker, GPIO_V2_LINE_SET_VALUES_IOCTL, &v);
}
static void path_for(char *path, size_t size, const char *dir, const char *name)
{
    if (snprintf(path, size, "%s/%s", dir, name) >= (int)size) {
        fprintf(stderr, "output path too long\n"); exit(2);
    }
}
static FILE *output(const char *dir, const char *name, const char *mode)
{
    char path[4096]; path_for(path, sizeof(path), dir, name);
    FILE *f = fopen(path, mode);
    if (!f) { perror(path); exit(2); }
    return f;
}
int main(int argc, char **argv)
{
    const char *vectors = NULL, *dir = NULL, *gate = NULL;
    const char *bus_path = "/dev/i2c-1", *chip = "/dev/gpiochip0";
    unsigned line = 23, address = 0x24, gap = 1000, per_byte = 23, snapshot_after = 0;
    int marker_only = 0;
    static const struct option options[] = {
        {"vectors",1,0,'v'}, {"output",1,0,'o'}, {"gate",1,0,'g'},
        {"bus",1,0,'b'}, {"chip",1,0,'c'}, {"line",1,0,'l'},
        {"address",1,0,'a'}, {"gap-us",1,0,'d'}, {"reply-us-per-byte",1,0,'p'},
        {"marker-only",0,0,'m'}, {"snapshot-after",1,0,'s'}, {0,0,0,0}
    };
    int opt;
    while ((opt = getopt_long(argc, argv, "", options, NULL)) != -1) {
        switch (opt) {
        case 'v': vectors=optarg; break; case 'o': dir=optarg; break;
        case 'g': gate=optarg; break; case 'b': bus_path=optarg; break;
        case 'c': chip=optarg; break; case 'l': line=strtoul(optarg,0,0); break;
        case 'a': address=strtoul(optarg,0,0); break;
        case 'd': gap=strtoul(optarg,0,0); break;
        case 'p': per_byte=strtoul(optarg,0,0); break;
        case 'm': marker_only=1; break;
        case 's': snapshot_after=strtoul(optarg,0,0); break; default:return 2;
        }
    }
    if (!dir || !gate || (!marker_only && !vectors) || address<8 || address>0x77)
        return 2;
    signal(SIGTERM, finish); signal(SIGINT, finish);
    int marker = marker_request(chip, line), bus = -1;
    FILE *input = NULL;
    if (!marker_only) {
        input = fopen(vectors, "rb");
        if (!input) { perror("vectors"); return 2; }
        bus = open(bus_path, O_RDWR);
        if (bus<0 || flock(bus, LOCK_EX|LOCK_NB) || ioctl(bus,I2C_RETRIES,0) ||
            ioctl(bus,I2C_SLAVE,address)) { perror("exclusive i2c setup"); return 2; }
    }
    FILE *ready = output(dir, "ready.json", "w");
    fprintf(ready, "{\"marker_released\":true,\"line\":%u,\"pid\":%ld}\n",line,(long)getpid());
    fclose(ready);
    while (!stopped && access(gate,F_OK)) delay_us(1000);
    if (stopped) goto cleanup;
    if (marker_only) {
        int rc=mark_error(marker); uint64_t stamp=now_ns();
        FILE *f=output(dir,"result.json","w");
        fprintf(f,"{\"marker_only\":true,\"marker_rc\":%d,\"marker_ns\":%llu}\n",rc,(unsigned long long)stamp);fclose(f);
        goto hold;
    }
    FILE *log=output(dir,"transfers.jsonl","w");
    setvbuf(log,NULL,_IOLBF,0);
    unsigned index=0; uint64_t completed=0,total=0; int error=0;
    uint64_t started=now_ns();
    while (!stopped) {
        unsigned char header[4], data[8192];
        size_t n=fread(header,1,4,input);
        if (!n && feof(input)) break;
        if (n!=4) { fprintf(stderr,"truncated vector header\n"); error=EINVAL; break; }
        uint32_t size=(uint32_t)header[0]|((uint32_t)header[1]<<8)|
                      ((uint32_t)header[2]<<16)|((uint32_t)header[3]<<24);
        if (!size || size>sizeof(data) || fread(data,1,size,input)!=size) {
            fprintf(stderr,"invalid/truncated vector\n"); error=EINVAL; break;
        }
        uint64_t began=now_ns();
        errno=0;
        ssize_t written=write(bus,data,size);
        int saved_errno=errno, marker_rc=0;
        uint64_t ended=now_ns(), marker_ns=0;
        int snapshot = snapshot_after && index + 1 == snapshot_after;
        if (written!=(ssize_t)size || snapshot) {
            /* First action after observing failure: latch a physical scope trigger.
             * No retry, reconnect, next request or printf precedes this ioctl.
             */
            marker_rc=mark_error(marker); marker_ns=now_ns();
            if (written!=(ssize_t)size) error=saved_errno ? saved_errno : EIO;
            FILE *f=output(dir,error ? "failed-request.bin" : "snapshot-request.bin","wb");
            fwrite(data,1,size,f);fclose(f);
        }
        fprintf(log,"{\"index\":%u,\"bytes\":%u,\"written\":%zd,\"errno\":%d,\"began_ns\":%llu,\"ended_ns\":%llu,\"marker_ns\":%llu,\"marker_rc\":%d}\n",
            index,size,written,saved_errno,(unsigned long long)began,
            (unsigned long long)ended,(unsigned long long)marker_ns,marker_rc);
        if (error) break;
        completed++;total+=size;index++;
        if (snapshot) break; /* Successful marker only after all selected write clocks. */
        if (completed%100==0) printf("PASS %llu writes, %llu bytes\n",(unsigned long long)completed,(unsigned long long)total);
        delay_us(gap+(uint64_t)per_byte*size);
    }
    fflush(log);fsync(fileno(log));fclose(log);
    FILE *result=output(dir,"result.json","w");
    fprintf(result,"{\"completed\":%llu,\"bytes\":%llu,\"error\":%d,\"failed_index\":%u,\"elapsed_ns\":%llu,\"stopped\":%s,\"snapshot_after\":%u}\n",
        (unsigned long long)completed,(unsigned long long)total,error,index,
        (unsigned long long)(now_ns()-started),stopped?"true":"false",snapshot_after);
    fflush(result);fsync(fileno(result));fclose(result);
    printf("RESULT completed=%llu error=%d\n",(unsigned long long)completed,error);fflush(stdout);
hold:
    while (!stopped) pause(); /* Keep marker/ownership latched until the harness stops us. */
cleanup:
    if (bus>=0) close(bus);
    if (input) fclose(input);
    close(marker);
    return 0;
}
