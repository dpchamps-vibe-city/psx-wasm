# psxw-emu-host. Built inside the psxw-oracle-emu image; LIBRETRO_INCLUDE
# points at the pinned core's libretro-common/include (libretro.h).

LIBRETRO_INCLUDE ?= /src/beetle/libretro-common/include
WIRE_INCLUDE ?= ../include
CFLAGS ?= -O2 -g
CFLAGS += -std=c11 -Wall -Wextra -Werror -I$(LIBRETRO_INCLUDE) -I$(WIRE_INCLUDE)
LDLIBS = -ldl

psxw-emu-host: host.o sha1.o
	$(CC) $(LDFLAGS) -o $@ $^ $(LDLIBS)

host.o: host.c sha1.h $(WIRE_INCLUDE)/psxw_oracle_wire.h
sha1.o: sha1.c sha1.h

clean:
	rm -f psxw-emu-host *.o

.PHONY: clean
