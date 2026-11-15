TOPDIR ?= $(CURDIR)
include $(DEVKITPRO)/libnx/switch_rules

ARCH := -mcpu=cortex-a57 -mtp=soft -fPIE
CFLAGS := -g -Wall -O3 -ffunction-sections $(ARCH) -I$(LIBNX)/include -I$(PORTLIBS)/include -D__SWITCH__
LDFLAGS = -specs=$(DEVKITPRO)/libnx/switch.specs -g $(ARCH) -Wl,--gc-sections -Wl,--build-id=sha1

LIB_RUST ?= /work/libtssm_switch.a
LIBS := $(LIB_RUST) -L$(PORTLIBS)/lib -L$(LIBNX)/lib -lglad -lEGL -lglapi -ldrm_nouveau -lSDL2 -lnx -lstdc++ -lm

OBJS := main.o gl_backend.o

all: tssm.nro main

main.o: main.c gl_backend.h
	$(CC) $(CFLAGS) -c $< -o $@

gl_backend.o: gl_backend.c gl_backend.h
	$(CC) $(CFLAGS) -c $< -o $@

tssm.elf: $(OBJS) $(LIB_RUST)
	$(CC) $(LDFLAGS) -o $@ $(OBJS) $(LIBS)
	python3 patch_armv9.py $@

control.nacp:
	nacptool --create "The SpongeBob Movie Game" "Heavy Iron / Recomp" "1.0.0" $@ --titleid=0100780012340000

tssm.nro: tssm.elf control.nacp icon.jpg
	elf2nro $< $@ --icon=icon.jpg --nacp=control.nacp

main: tssm.elf
	elf2nso $< $@

clean:
	rm -f *.o *.elf *.nro main control.nacp
