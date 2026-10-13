#include <stddef.h>

struct Config {
    int verbose;
    size_t buffer_size;
};

static const int DEFAULT_SIZE = 1024;

size_t config_size(const struct Config* cfg) {
    return cfg->buffer_size ? cfg->buffer_size : DEFAULT_SIZE;
}
