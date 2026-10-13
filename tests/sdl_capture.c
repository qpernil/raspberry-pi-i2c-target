/* Test-only SDL readback shim for the real virtual-display executable.
 * cc -shared -fPIC -Wall -Wextra -Werror tests/sdl_capture.c \
 *    $(sdl2-config --cflags --libs) -ldl -o /tmp/display-capture.so
 * SDL_VIDEODRIVER=dummy VIRTUAL_DISPLAY_CAPTURE=/tmp/display-frame.bin \
 * LD_PRELOAD=/tmp/display-capture.so sudo ... (pass environment explicitly).
 * Captures canonical 128x64 page bytes after actual SDL pixel conversion.
 * GPU readback synchronizes rendering and can perturb real I2C FIFO servicing.
 * Use for isolated pixel diagnostics, not the baseline hardware transfer test.
 */
#define _GNU_SOURCE
#include <SDL2/SDL.h>
#include <dlfcn.h>
#include <stdio.h>
#include <stdlib.h>
#include <stdint.h>
#include <string.h>

void SDL_RenderPresent(SDL_Renderer *renderer)
{
    void (*present)(SDL_Renderer *) = dlsym(RTLD_NEXT, "SDL_RenderPresent");
    const char *path = getenv("VIRTUAL_DISPLAY_CAPTURE");
    if (!present) abort();
    if (path) {
        int width, height;
        if (SDL_GetRendererOutputSize(renderer, &width, &height) || width < 128 || height < 64)
            abort();
        uint32_t *pixels = malloc((size_t)width * height * sizeof(*pixels));
        if (!pixels || SDL_RenderReadPixels(renderer, NULL, SDL_PIXELFORMAT_ARGB8888,
                                            pixels, width * (int)sizeof(*pixels))) abort();
        unsigned char frame[1024] = {0};
        for (int y = 0; y < 64; y++) {
            for (int x = 0; x < 128; x++) {
                int py = ((2 * y + 1) * height) / 128;
                int px = ((2 * x + 1) * width) / 256;
                uint32_t color = pixels[py * width + px];
                if (color == 0xfff2f2f2U) {
                    int sy = 63 - y, sx = 127 - x;
                    frame[(sy / 8) * 128 + sx] |= 1U << (sy % 8);
                } else if (color != 0xff000000U) {
                    fprintf(stderr, "unexpected SDL pixel %08x\n", color);
                    abort();
                }
            }
        }
        free(pixels);
        char *temporary;
        if (asprintf(&temporary, "%s.tmp", path) < 0) abort();
        FILE *file = fopen(temporary, "wb");
        if (!file || fwrite(frame, 1, sizeof(frame), file) != sizeof(frame)) abort();
        if (fclose(file) || rename(temporary, path)) abort();
        free(temporary);
    }
    present(renderer);
}
