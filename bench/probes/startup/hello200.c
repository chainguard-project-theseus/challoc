/* bench/probes/startup/hello200.c: the input of the cc startup row
 * (bench/drivers/startup.sh): 200 lines of ordinary C, so `gcc -O2 -c`
 * is a short cc1 + as run. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

struct item {
    int id;
    double weight;
    char name[32];
};

static int f0(const struct item *it, int n) {
    int acc = 0;
    for (int k = 0; k < n; k++) acc += it[k].id * (int)it[k].weight + (int)strlen(it[k].name);
    return acc;
}

static int f1(const struct item *it, int n) {
    int acc = 1;
    for (int k = 0; k < n; k++) acc += it[k].id * (int)it[k].weight + (int)strlen(it[k].name);
    return acc;
}

static int f2(const struct item *it, int n) {
    int acc = 2;
    for (int k = 0; k < n; k++) acc += it[k].id * (int)it[k].weight + (int)strlen(it[k].name);
    return acc;
}

static int f3(const struct item *it, int n) {
    int acc = 3;
    for (int k = 0; k < n; k++) acc += it[k].id * (int)it[k].weight + (int)strlen(it[k].name);
    return acc;
}

static int f4(const struct item *it, int n) {
    int acc = 4;
    for (int k = 0; k < n; k++) acc += it[k].id * (int)it[k].weight + (int)strlen(it[k].name);
    return acc;
}

static int f5(const struct item *it, int n) {
    int acc = 5;
    for (int k = 0; k < n; k++) acc += it[k].id * (int)it[k].weight + (int)strlen(it[k].name);
    return acc;
}

static int f6(const struct item *it, int n) {
    int acc = 6;
    for (int k = 0; k < n; k++) acc += it[k].id * (int)it[k].weight + (int)strlen(it[k].name);
    return acc;
}

static int f7(const struct item *it, int n) {
    int acc = 7;
    for (int k = 0; k < n; k++) acc += it[k].id * (int)it[k].weight + (int)strlen(it[k].name);
    return acc;
}

static int f8(const struct item *it, int n) {
    int acc = 8;
    for (int k = 0; k < n; k++) acc += it[k].id * (int)it[k].weight + (int)strlen(it[k].name);
    return acc;
}

static int f9(const struct item *it, int n) {
    int acc = 9;
    for (int k = 0; k < n; k++) acc += it[k].id * (int)it[k].weight + (int)strlen(it[k].name);
    return acc;
}

static int f10(const struct item *it, int n) {
    int acc = 10;
    for (int k = 0; k < n; k++) acc += it[k].id * (int)it[k].weight + (int)strlen(it[k].name);
    return acc;
}

static int f11(const struct item *it, int n) {
    int acc = 11;
    for (int k = 0; k < n; k++) acc += it[k].id * (int)it[k].weight + (int)strlen(it[k].name);
    return acc;
}

static int f12(const struct item *it, int n) {
    int acc = 12;
    for (int k = 0; k < n; k++) acc += it[k].id * (int)it[k].weight + (int)strlen(it[k].name);
    return acc;
}

static int f13(const struct item *it, int n) {
    int acc = 13;
    for (int k = 0; k < n; k++) acc += it[k].id * (int)it[k].weight + (int)strlen(it[k].name);
    return acc;
}

static int f14(const struct item *it, int n) {
    int acc = 14;
    for (int k = 0; k < n; k++) acc += it[k].id * (int)it[k].weight + (int)strlen(it[k].name);
    return acc;
}

static int f15(const struct item *it, int n) {
    int acc = 15;
    for (int k = 0; k < n; k++) acc += it[k].id * (int)it[k].weight + (int)strlen(it[k].name);
    return acc;
}

static int f16(const struct item *it, int n) {
    int acc = 16;
    for (int k = 0; k < n; k++) acc += it[k].id * (int)it[k].weight + (int)strlen(it[k].name);
    return acc;
}

static int f17(const struct item *it, int n) {
    int acc = 17;
    for (int k = 0; k < n; k++) acc += it[k].id * (int)it[k].weight + (int)strlen(it[k].name);
    return acc;
}

static int f18(const struct item *it, int n) {
    int acc = 18;
    for (int k = 0; k < n; k++) acc += it[k].id * (int)it[k].weight + (int)strlen(it[k].name);
    return acc;
}

static int f19(const struct item *it, int n) {
    int acc = 19;
    for (int k = 0; k < n; k++) acc += it[k].id * (int)it[k].weight + (int)strlen(it[k].name);
    return acc;
}

static int f20(const struct item *it, int n) {
    int acc = 20;
    for (int k = 0; k < n; k++) acc += it[k].id * (int)it[k].weight + (int)strlen(it[k].name);
    return acc;
}

static int f21(const struct item *it, int n) {
    int acc = 21;
    for (int k = 0; k < n; k++) acc += it[k].id * (int)it[k].weight + (int)strlen(it[k].name);
    return acc;
}

static int f22(const struct item *it, int n) {
    int acc = 22;
    for (int k = 0; k < n; k++) acc += it[k].id * (int)it[k].weight + (int)strlen(it[k].name);
    return acc;
}

static int f23(const struct item *it, int n) {
    int acc = 23;
    for (int k = 0; k < n; k++) acc += it[k].id * (int)it[k].weight + (int)strlen(it[k].name);
    return acc;
}

static int f24(const struct item *it, int n) {
    int acc = 24;
    for (int k = 0; k < n; k++) acc += it[k].id * (int)it[k].weight + (int)strlen(it[k].name);
    return acc;
}

static int f25(const struct item *it, int n) {
    int acc = 25;
    for (int k = 0; k < n; k++) acc += it[k].id * (int)it[k].weight + (int)strlen(it[k].name);
    return acc;
}

int main(int argc, char **argv) {
    struct item it[4] = {{1, 1.5, "a"}, {2, 2.5, "bb"}, {3, 3.5, "ccc"}, {4, 4.5, "dddd"}};
    int n = argc > 1 ? atoi(argv[1]) : 4;
    int s = 0;
    s += f0(it, n);
    s += f1(it, n);
    s += f2(it, n);
    s += f3(it, n);
    s += f4(it, n);
    s += f5(it, n);
    s += f6(it, n);
    s += f7(it, n);
    s += f8(it, n);
    s += f9(it, n);
    s += f10(it, n);
    s += f11(it, n);
    s += f12(it, n);
    s += f13(it, n);
    s += f14(it, n);
    s += f15(it, n);
    s += f16(it, n);
    s += f17(it, n);
    s += f18(it, n);
    s += f19(it, n);
    s += f20(it, n);
    s += f21(it, n);
    s += f22(it, n);
    s += f23(it, n);
    s += f24(it, n);
    s += f25(it, n);
    printf("%d\n", s);
    return 0;
}
