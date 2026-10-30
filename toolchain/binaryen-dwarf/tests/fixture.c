typedef unsigned int u32;
__attribute__((import_module("psx"), import_name("bios")))
extern int bios(int);
volatile int observed;

__attribute__((noinline)) int leaf(int a) {
    int local = a * 3 + 7;
    observed = local;
    return local ^ 0x55;
}

__attribute__((noinline)) int worker(int input) {
    int before = input + 11;
    volatile int stack_value = before * 2;
    int first = leaf(before);
    int resumed = bios(first);
    int after = before + resumed + stack_value;
    observed = after;
    return after;
}

__attribute__((noinline)) int root(int input) {
    int left = worker(input);
    int right = worker(input + 1);
    return left + right;
}

__attribute__((noinline)) int branchy(int input) {
    int saved = input * 13;
    int result;
    if (input & 1) {
        result = bios(input + 2);
        observed = result + saved;
    } else {
        result = bios(input - 2);
        observed = result - saved;
    }
    for (int i = 0; i < 2; i++) {
        result += bios(result + i);
    }
    return result + saved;
}

struct Sample { int count; float weight; double scale; long long bits; };
volatile struct Sample observed_sample;

__attribute__((noinline)) int typed(int input) {
    volatile struct Sample sample = {input + 3, 1.5f, 2.25, 0x100000002LL + input};
    struct Sample parts = {input * 2, 3.5f, 4.25, 0x200000003LL + input};
    double ratio = input + 0.5;
    long long big = ((long long)input << 33) + 7;
    // Positive storage-backed type control, not an -O0 compiler workaround.
    // Empty aggregate pieces / folded low-bit-only expressions have separate
    // fail-closed importer tests; they must not be read as zero-filled values.
#ifndef LLVM_INPUT_REPRO
    __asm__ __volatile__("" : : "r"(&sample), "r"(&parts), "r"(&ratio), "r"(&big) : "memory");
#endif
    int resumed = bios(input);
    observed_sample = sample; /* typed-stop */
    return resumed + sample.count + (int)sample.weight + (int)sample.scale +
        (int)sample.bits + parts.count + (int)parts.weight + (int)parts.scale +
        (int)parts.bits + (int)ratio + (int)big;
}

/* `dead`'s last use precedes the suspension, so Asyncify doesn't save its
   local. After a resume it must be unavailable, not the re-entered frame's
   zero. */
__attribute__((noinline)) int dead_across(int input) {
    int dead = input * 7 + 3;
    observed = dead;
    observed = dead ^ 0x55;
    int resumed = bios(input);
    observed = resumed; /* dead-stop */
    return resumed;
}
