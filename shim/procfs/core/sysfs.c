// The emulated sysfs slices: /sys/class/net (sysinfo's network counters plus
// the interface-facts trio), /sys/class/power_supply (starship-battery) and
// /sys/devices/system/cpu (cpufreq and topology). Each slice is one walk
// that emits (relative path, content) pairs from NT data cached for a
// throttle window. The same walk serves two carriers: the materializer
// writes the tree once and rewrites it only when the *set of files*
// changes (an interface or a battery came or went), since the on-disk copy
// is for stat() and listings and its values go stale by design; and the
// open interception picks one file's current content out of the walk for a
// scratch descriptor, the way /proc content is served. Before this split
// every value file was rewritten each second, and each rewrite cost a
// write plus the filter drivers' scan on the reader's next open.

#define _COSMO_SOURCE // for libc/dce.h's IsWindows() and g_fds

#include <dirent.h>
#include <errno.h>
#include <limits.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdarg.h>
#include <stdatomic.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>
#include <libc/calls/internal.h>
#include <libc/dce.h>
#include <libc/nt/files.h>
#include <libc/runtime/runtime.h>

#include "core.h"

typedef void (*sysfs_emit)(void *ctx, const char *rel, const char *val,
                           size_t n);
static void ensure_sysfs_dmi(void);

static void sysfs_net_walk(sysfs_emit emit, void *ctx) {
    static struct pfs_ifstat ifs[32];
    static int n;
    static int64_t last;
    int64_t t = pfs_now_ms();
    if (!last || t - last >= NET_MS) {
        n = pfs_net_ifstats(ifs, 32);
        last = t;
    }
    char rel[200], val[32];
    for (int i = 0; i < n; i++) {
        struct pfs_ifstat *s = &ifs[i];
        static const char *const names[] = {"rx_bytes",   "tx_bytes",
                                            "rx_packets", "tx_packets",
                                            "rx_errors",  "tx_errors",
                                            "rx_dropped", "tx_dropped"};
        const uint64_t vals[] = {s->rx_bytes, s->tx_bytes, s->rx_pkts,
                                 s->tx_pkts,  s->rx_errs,  s->tx_errs,
                                 s->rx_drop,  s->tx_drop};
        for (int k = 0; k < 8; k++) {
            int m = snprintf(val, sizeof val, "%llu\n",
                             (unsigned long long)vals[k]);
            snprintf(rel, sizeof rel, "%s/statistics/%s", s->name, names[k]);
            emit(ctx, rel, val, (size_t)m);
        }
        int m = snprintf(val, sizeof val, "%u\n", s->mtu);
        snprintf(rel, sizeof rel, "%s/mtu", s->name);
        emit(ctx, rel, val, (size_t)m);
        snprintf(rel, sizeof rel, "%s/operstate", s->name);
        emit(ctx, rel, s->up ? "up\n" : "down\n", s->up ? 3 : 5);
        // the interface-facts trio readers reach for next: hardware
        // address (all zeros where there is none, as loopback reads on
        // Linux), carrier, and the negotiated speed in Mb/s (-1 when down
        // or unknown, the kernel's own answer in that state)
        char mac[20];
        m = 0;
        for (int k = 0; k < 6; k++)
            m += snprintf(mac + m, sizeof mac - (size_t)m, "%02x%s",
                          k < (int)s->maclen ? s->mac[k] : 0,
                          k < 5 ? ":" : "\n");
        snprintf(rel, sizeof rel, "%s/address", s->name);
        emit(ctx, rel, mac, (size_t)m);
        snprintf(rel, sizeof rel, "%s/carrier", s->name);
        emit(ctx, rel, s->up ? "1\n" : "0\n", 2);
        if (s->up && s->speed_mbps)
            m = snprintf(val, sizeof val, "%llu\n",
                         (unsigned long long)s->speed_mbps);
        else
            m = snprintf(val, sizeof val, "-1\n");
        snprintf(rel, sizeof rel, "%s/speed", s->name);
        emit(ctx, rel, val, (size_t)m);
    }
}

// /sys/class/power_supply. Battery readers hard-require type, status, the
// energy_* trio, power_now and voltage_now per battery; everything else has
// a fallback. Design capacity, cycle count and chemistry come off the
// battery device itself; when that query has no answer, design falls back
// to full (health then reads 100%). Voltage NT never says, so it is a
// nominal constant that only feeds the display field.
static void sysfs_power_walk(sysfs_emit emit, void *ctx) {
    static struct pfs_batt b;
    static bool have;
    static int64_t last;
    int64_t t = pfs_now_ms();
    if (!last || t - last >= 1000) {
        have = pfs_battery(&b);
        last = t;
    }
    if (!have) return;
    char rel[64], buf[64];
    emit(ctx, "ADP0/type", "Mains\n", 6);
    emit(ctx, "ADP0/online", b.ac ? "1\n" : "0\n", 2);
    if (!b.present) return; // a battery that went away takes its files along
    struct {
        const char *name, *sval;
        uint64_t nval;
    } files[] = {
        {"type", "Battery", 0},
        {"present", 0, 1},
        {"status", 0, 0}, // filled below
        {"capacity", 0, b.max_mwh ? b.rem_mwh * 100ull / b.max_mwh : 0},
        {"energy_now", 0, (uint64_t)b.rem_mwh * 1000},
        {"energy_full", 0, (uint64_t)b.max_mwh * 1000},
        {"energy_full_design", 0,
         (uint64_t)(b.design_mwh ? b.design_mwh : b.max_mwh) * 1000},
        {"cycle_count", 0, b.cycles},
        {"power_now", 0, (uint64_t)b.rate_mw * 1000},
        {"voltage_now", 0, 11400000}, // nominal; NT does not say
        {"model_name", "APE Battery", 0},
        {"manufacturer", "rust-ape", 0},
        {"technology", 0, 0}, // filled below
    };
    files[2].sval = b.charging      ? "Charging"
                    : b.discharging ? "Discharging"
                    : b.rem_mwh >= b.max_mwh ? "Full"
                                             : "Unknown";
    size_t itech = sizeof files / sizeof files[0] - 1;
    files[itech].sval = !strncmp(b.chem, "NiCd", 4)   ? "NiCd"
                        : !strncmp(b.chem, "NiMH", 4) ? "NiMH"
                        : !strncmp(b.chem, "LiP", 3)  ? "Li-poly"
                                                      : "Li-ion";
    for (size_t i = 0; i < sizeof files / sizeof files[0]; i++) {
        int m = files[i].sval
                    ? snprintf(buf, sizeof buf, "%s\n", files[i].sval)
                    : snprintf(buf, sizeof buf, "%llu\n",
                               (unsigned long long)files[i].nval);
        snprintf(rel, sizeof rel, "BAT0/%s", files[i].name);
        emit(ctx, rel, buf, (size_t)m);
    }
}

// /sys/devices/system/cpu: the cpufreq files sysinfo reads its frequencies
// from, plus the online/possible/present range files.
static void sysfs_cpu_walk(sysfs_emit emit, void *ctx) {
    static uint32_t cur[64], max[64];
    static uint8_t core[64], pkg[64];
    static int n, topo;
    static int64_t last;
    int64_t t = pfs_now_ms();
    if (!last || t - last >= 1000) {
        n = pfs_cpu_mhz(cur, max, 64);
        topo = pfs_cpu_topology(core, pkg, 64);
        last = t;
    }
    if (!n) return;
    char rel[64], buf[64];
    int m = snprintf(buf, sizeof buf, "0-%d\n", n - 1);
    static const char *const ranges[] = {"online", "possible", "present"};
    for (int i = 0; i < 3; i++) emit(ctx, ranges[i], buf, (size_t)m);
    for (int i = 0; i < n; i++) {
        struct { const char *name; uint32_t mhz; } f[] = {
            {"scaling_cur_freq", cur[i]},
            {"scaling_max_freq", max[i]},
            {"cpuinfo_max_freq", max[i]},
            {"scaling_min_freq", 0}, // NT has no floor figure; 0 is honest
            {"cpuinfo_min_freq", 0},
        };
        for (int k = 0; k < 5; k++) {
            m = snprintf(buf, sizeof buf, "%u\n", f[k].mhz * 1000); // kHz
            snprintf(rel, sizeof rel, "cpu%d/cpufreq/%s", i, f[k].name);
            emit(ctx, rel, buf, (size_t)m);
        }
        struct { const char *name; int val; } tp[] = {
            {"core_id", topo && i < topo ? core[i] : i},
            {"physical_package_id", topo && i < topo ? pkg[i] : 0},
        };
        for (int k = 0; k < 2; k++) {
            m = snprintf(buf, sizeof buf, "%d\n", tp[k].val);
            snprintf(rel, sizeof rel, "cpu%d/topology/%s", i, tp[k].name);
            emit(ctx, rel, buf, (size_t)m);
        }
    }
}

// The materializing carrier of a walk: mkdir -p for each file's parents,
// then the file. Only run when the set of names changed.
struct mat_ctx {
    const char *base;
};

static void mat_emit(void *vctx, const char *rel, const char *val, size_t n) {
    struct mat_ctx *c = vctx;
    char path[600];
    int len = snprintf(path, sizeof path, "%s/%s", c->base, rel);
    if (len <= 0 || (size_t)len >= sizeof path) return;
    for (char *p = path + strlen(c->base) + 1; *p; p++) {
        if (*p != '/') continue;
        *p = 0;
        mkdir(path, 0755);
        *p = '/';
    }
    pc_write_file(path, val, n);
}

static void names_emit(void *vctx, const char *rel, const char *val,
                       size_t n) {
    (void)val, (void)n;
    *(uint64_t *)vctx = pc_fnv64(rel, strlen(rel) + 1, *(uint64_t *)vctx);
}

static void ensure_sysfs_slice(const char *dir,
                               void (*walk)(sysfs_emit, void *),
                               uint64_t *digest, int64_t *last, int64_t ms) {
    int64_t t = pfs_now_ms();
    if (*last && t - *last < ms) return;
    *last = t;
    uint64_t d = 0xcbf29ce484222325ull;
    walk(names_emit, &d);
    if (d == *digest) return;
    *digest = d;
    char base[600];
    snprintf(base, sizeof base, "%s/%s", pc_root, dir);
    pc_rm_rf(base); // the shape changed: start from the new set
    mkdir(base, 0755);
    struct mat_ctx c = {base};
    walk(mat_emit, &c);
}

static void ensure_sysfs_net(void) {
    static uint64_t digest;
    static int64_t last;
    ensure_sysfs_slice("sysfs-net", sysfs_net_walk, &digest, &last, NET_MS);
}

static void ensure_sysfs_power(void) {
    static uint64_t digest;
    static int64_t last;
    ensure_sysfs_slice("sysfs-power", sysfs_power_walk, &digest, &last, 1000);
}

static void ensure_sysfs_cpu(void) {
    static uint64_t digest;
    static int64_t last;
    ensure_sysfs_slice("sysfs-cpu", sysfs_cpu_walk, &digest, &last, 1000);
}

// The virtual carrier of a walk: one file's current content.
struct pick_ctx {
    const char *rel;
    struct pfs_buf *out;
    bool found;
};

static void pick_emit(void *vctx, const char *rel, const char *val, size_t n) {
    struct pick_ctx *c = vctx;
    if (c->found || strcmp(rel, c->rel)) return;
    c->found = true;
    pfs_put(c->out, val, n);
}

// The emulated slices of sysfs. `walk` is 0 for a slice served only from
// disk (dmi: fixed for the machine's uptime).
static const struct {
    struct pc_sysfs s;
    void (*walk)(sysfs_emit, void *);
} g_sysfs[] = {
    {{"/sys/class/net", 14, ensure_sysfs_net, "sysfs-net"}, sysfs_net_walk},
    {{"/sys/class/power_supply", 23, ensure_sysfs_power, "sysfs-power"},
     sysfs_power_walk},
    {{"/sys/devices/system/cpu", 23, ensure_sysfs_cpu, "sysfs-cpu"},
     sysfs_cpu_walk},
    {{"/sys/class/dmi", 14, ensure_sysfs_dmi, "sysfs-dmi"}, 0},
    {{"/sys/devices/virtual/dmi", 24, ensure_sysfs_dmi, "sysfs-dmi"}, 0},
};
#define NSYSFS (sizeof g_sysfs / sizeof g_sysfs[0])

// The slice `path` is the root of or lies under, or 0.
const struct pc_sysfs *pc_sysfs_match(const char *path) {
    for (size_t i = 0; i < NSYSFS; i++) {
        const struct pc_sysfs *s = &g_sysfs[i].s;
        if (strncmp(path, s->prefix, s->len)) continue;
        if (path[s->len] && path[s->len] != '/') continue;
        return s;
    }
    return 0;
}

// Content of a sysfs path, when a walk emits it. Called under pc_lock.
bool pc_gen_sysfs(const char *path, struct pfs_buf *out) {
    for (size_t i = 0; i < NSYSFS; i++) {
        const struct pc_sysfs *s = &g_sysfs[i].s;
        if (!g_sysfs[i].walk || strncmp(path, s->prefix, s->len)) continue;
        if (path[s->len] != '/') continue;
        struct pick_ctx c = {path + s->len + 1, out, false};
        g_sysfs[i].walk(pick_emit, &c);
        return c.found;
    }
    return false;
}

// /sys/class/dmi/id, also served under /sys/devices/virtual/dmi/id where
// the real files live (sysinfo reads that spelling; the class path is a
// symlink to it on Linux). Fixed for the machine's uptime: written once.
static void ensure_sysfs_dmi(void) {
    static bool done;
    if (done) return;
    done = true;

    char base[600], path[600], buf[600];
    snprintf(base, sizeof base, "%s/sysfs-dmi", pc_root);
    mkdir(base, 0755);
    snprintf(base, sizeof base, "%s/sysfs-dmi/id", pc_root);
    mkdir(base, 0755);

    struct pfs_dmi d;
    if (!pfs_dmi(&d)) return;

    const struct { bool have; const char *name; const char *val; } files[] = {
        {d.have_bios, "bios_vendor", d.bios_vendor},
        {d.have_bios, "bios_version", d.bios_version},
        {d.have_bios, "bios_date", d.bios_date},
        {d.have_sys, "sys_vendor", d.sys_vendor},
        {d.have_sys, "product_name", d.product_name},
        {d.have_sys, "product_version", d.product_version},
        {d.have_sys, "product_sku", d.product_sku},
        {d.have_sys, "product_family", d.product_family},
        {d.have_board, "board_vendor", d.board_vendor},
        {d.have_board, "board_name", d.board_name},
        {d.have_board, "board_version", d.board_version},
        {d.have_board, "board_asset_tag", d.board_asset_tag},
        {d.have_chassis, "chassis_vendor", d.chassis_vendor},
        {d.have_chassis, "chassis_version", d.chassis_version},
    };
    for (size_t i = 0; i < sizeof files / sizeof files[0]; i++) {
        if (!files[i].have) continue;
        int m = snprintf(buf, sizeof buf, "%s\n", files[i].val);
        snprintf(path, sizeof path, "%s/%s", base, files[i].name);
        pc_write_file(path, buf, (size_t)m);
    }
    if (d.have_chassis) {
        int m = snprintf(buf, sizeof buf, "%d\n", d.chassis_type);
        snprintf(path, sizeof path, "%s/chassis_type", base);
        pc_write_file(path, buf, (size_t)m);
    }
    // modalias: every field strung together the way the kernel spells it,
    // spaces and colons dropped from the values
    const struct { const char *tag; const char *val; } parts[] = {
        {"bvn", d.bios_vendor},   {"bvr", d.bios_version},
        {"bd", d.bios_date},      {"svn", d.sys_vendor},
        {"pn", d.product_name},   {"pvr", d.product_version},
        {"rvn", d.board_vendor},  {"rn", d.board_name},
        {"rvr", d.board_version}, {"cvn", d.chassis_vendor},
        {"cvr", d.chassis_version},
    };
    size_t k = snprintf(buf, sizeof buf, "dmi:");
    for (size_t i = 0; i < sizeof parts / sizeof parts[0]; i++) {
        for (const char *c = parts[i].tag; *c && k < sizeof buf - 3; c++)
            buf[k++] = *c;
        for (const char *c = parts[i].val; *c && k < sizeof buf - 3; c++)
            if (*c != ' ' && *c != ':') buf[k++] = *c;
        buf[k++] = ':';
        if (i == 9 && k < sizeof buf - 8)
            k += (size_t)snprintf(buf + k, sizeof buf - k, "ct%d:",
                                  d.chassis_type);
    }
    buf[k++] = '\n';
    snprintf(path, sizeof path, "%s/modalias", base);
    pc_write_file(path, buf, k);
}
