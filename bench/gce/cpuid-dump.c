/*
 * cpuid-dump: the raw registers the feature report decodes, so a matrix
 * cell's capture can become a host-test fixture in src/sys/cpu.rs
 * (Phase 2.5 task 4). Compiled and run inside the dev container by
 * bench/gce/matrix-cell.sh; no dependency beyond libc.
 *
 * x86_64: leaves 0, 1, 7.0, 7.1, the deterministic cache parameters
 * (leaf 4 and 0x8000001D, subleaves until a null type), 0x80000000,
 * 0x80000008 and 0x80000021 (AMD's UAI in EAX[7]), one line each:
 *   cpuid leaf=0x00000007 sub=1 eax=0x00001c30 ebx=0x00000000 ecx=... edx=...
 * A leaf above the range the CPU reports is printed as `absent`.
 *
 * aarch64: AT_HWCAP and AT_HWCAP2 from getauxval, and MIDR_EL1 from sysfs:
 *   hwcap=0xefffffff hwcap2=0x3f3ff midr=0x410fd4f1
 */
#include <stdio.h>
#include <stdint.h>

#if defined(__x86_64__)
#include <cpuid.h>

static void leaf(uint32_t l, uint32_t sub, uint32_t max)
{
    if (l > max) {
        printf("cpuid leaf=0x%08x sub=%u absent (max 0x%08x)\n", l, sub, max);
        return;
    }
    uint32_t a, b, c, d;
    __cpuid_count(l, sub, a, b, c, d);
    printf("cpuid leaf=0x%08x sub=%u eax=0x%08x ebx=0x%08x ecx=0x%08x edx=0x%08x\n", l, sub, a, b, c, d);
}

/* Cache parameter subleaves: stop after the first null type (EAX[4:0] == 0). */
static void caches(uint32_t l, uint32_t max)
{
    if (l > max) {
        printf("cpuid leaf=0x%08x sub=0 absent (max 0x%08x)\n", l, max);
        return;
    }
    for (uint32_t s = 0; s < 8; s++) {
        uint32_t a, b, c, d;
        __cpuid_count(l, s, a, b, c, d);
        printf("cpuid leaf=0x%08x sub=%u eax=0x%08x ebx=0x%08x ecx=0x%08x edx=0x%08x\n", l, s, a, b, c, d);
        if ((a & 0x1f) == 0)
            break;
    }
}

int main(void)
{
    uint32_t a, b, c, d;
    __cpuid(0, a, b, c, d);
    uint32_t max = a;
    char vendor[13];
    ((uint32_t *)vendor)[0] = b;
    ((uint32_t *)vendor)[1] = d;
    ((uint32_t *)vendor)[2] = c;
    vendor[12] = 0;
    __cpuid(0x80000000u, a, b, c, d);
    uint32_t xmax = a;
    printf("vendor=%s max_leaf=0x%08x max_ext_leaf=0x%08x\n", vendor, max, xmax);
    leaf(0, 0, max);
    leaf(1, 0, max);
    leaf(7, 0, max);
    if (max >= 7) {
        __cpuid_count(7, 0, a, b, c, d);
        if (a >= 1)
            leaf(7, 1, max);
        else
            printf("cpuid leaf=0x00000007 sub=1 absent (subleaf 0 reports %u subleaves)\n", a);
    }
    caches(4, max);
    leaf(0x80000000u, 0, xmax);
    leaf(0x80000008u, 0, xmax);
    caches(0x8000001Du, xmax);
    leaf(0x80000021u, 0, xmax);
    return 0;
}

#elif defined(__aarch64__)
#include <sys/auxv.h>

int main(void)
{
    unsigned long hwcap = getauxval(AT_HWCAP), hwcap2 = getauxval(AT_HWCAP2);
    unsigned long long midr = 0;
    FILE *f = fopen("/sys/devices/system/cpu/cpu0/regs/identification/midr_el1", "r");
    if (f) {
        if (fscanf(f, "%llx", &midr) != 1)
            midr = 0;
        fclose(f);
    }
    printf("hwcap=0x%lx hwcap2=0x%lx midr=0x%llx\n", hwcap, hwcap2, midr);
    return 0;
}

#else
int main(void)
{
    printf("cpuid-dump: unsupported architecture\n");
    return 0;
}
#endif
