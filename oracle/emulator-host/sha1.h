#ifndef PSXW_SHA1_H
#define PSXW_SHA1_H

#include <stddef.h>
#include <stdint.h>

/* Lowercase hex SHA-1 of `data`, NUL-terminated. */
void sha1_hex(const uint8_t *data, size_t len, char out[41]);

#endif
