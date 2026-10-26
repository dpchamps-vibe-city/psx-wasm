# psxw runner: the PS1 program that executes oracle vectors on the chips.
# Built in the psxw-psx-sdk image with PSn00bSDK's toolchain and elf2x; it
# links none of PSn00bSDK's libraries (start.s replaces the SDK runtime).

cmake_minimum_required(VERSION 3.21)

project(psxw-runner LANGUAGES C ASM)

set(PSXW_INCLUDE "${CMAKE_CURRENT_LIST_DIR}/../include" CACHE PATH "Directory holding psxw_oracle_wire.h")

psn00bsdk_add_executable(
    psxw-runner NOGPREL
    start.s
    main.c
    gpu.c
    gte.c
    spu.c
    bios.c
    bios_call.s
)
target_include_directories(psxw-runner PRIVATE ${PSXW_INCLUDE})
target_compile_options(psxw-runner PRIVATE $<$<COMPILE_LANGUAGE:C>:-Wall -Wextra -Werror -std=gnu11>)
