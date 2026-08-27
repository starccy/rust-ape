// Power facts for the sysfs slices, the battery behind
// /sys/class/power_supply and the per-core frequencies behind
// /sys/devices/system/cpu and /proc/cpuinfo's MHz column. Both come from
// CallNtPowerInformation (powrprof.dll, resolved dynamically), which a probe
// verified on this vintage. Class 5 answers a 32-byte SYSTEM_BATTERY_STATE
// and class 11 fills one 24-byte record per processor.

#define _COSMO_SOURCE // for libc/dce.h's IsWindows()

#include <stdbool.h>
#include <stdint.h>
#include <string.h>
#include <libc/dce.h>
#include <libc/nt/createfile.h>
#include <libc/nt/enum/creationdisposition.h>
#include <libc/nt/enum/filesharemode.h>
#include <libc/nt/files.h>
#include <libc/nt/runtime.h>
#include <libc/nt/struct/guid.h>
#include <libc/nt/struct/systeminfo.h>
#include <libc/nt/systeminfo.h>
#include <libc/nt/thunk/msabi.h>

#include "procfs.h"

typedef int32_t (__msabi *CallNtPowerF)(int, void *, uint32_t, void *,
                                        uint32_t);

static CallNtPowerF power_fn(void) {
    static CallNtPowerF f;
    if (!f)
        f = (CallNtPowerF)pfs_sym(u"powrprof.dll", "CallNtPowerInformation");
    return f;
}

// Design capacity, cycle count and chemistry come off the battery device
// itself, since CallNtPowerInformation's aggregate carries no design
// figure. The device is found through the battery interface class; the
// figures are constant for a battery, so one successful query is cached
// for good.
typedef uint32_t (__msabi *CmListSizeF)(uint32_t *, const struct NtGuid *,
                                        const char16_t *, uint32_t);
typedef uint32_t (__msabi *CmListF)(const struct NtGuid *, const char16_t *,
                                    char16_t *, uint32_t, uint32_t);

static void batt_design(struct pfs_batt *out) {
    static bool have;
    static uint32_t design, cycles;
    static char chem[5]; // statics guarded by the carrier's lock
    if (!have) {
        static const struct NtGuid guid = { // GUID_DEVINTERFACE_BATTERY
            0x72631e54, 0x78A4, 0x11d0,
            {0xbc, 0xf7, 0x00, 0xaa, 0x00, 0xb7, 0xb3, 0x2a}};
        CmListSizeF lsz = (CmListSizeF)pfs_sym(
            u"cfgmgr32.dll", "CM_Get_Device_Interface_List_SizeW");
        CmListF lst =
            (CmListF)pfs_sym(u"cfgmgr32.dll", "CM_Get_Device_Interface_ListW");
        static char16_t paths[1024];
        uint32_t need = 0;
        if (!lsz || !lst) return;
        if (lsz(&need, &guid, 0, 0 /* present only */) || need < 2 ||
            need > 1024)
            return;
        if (lst(&guid, 0, paths, need, 0)) return;
        int64_t h = CreateFile(paths, 0xC0000000u /* GENERIC_READ|WRITE */,
                               kNtFileShareRead | kNtFileShareWrite, 0,
                               kNtOpenExisting, 0, 0);
        if (h == -1) return;
        uint32_t wait = 0, tag = 0, got = 0;
        if (DeviceIoControl(h, 0x294040 /* IOCTL_BATTERY_QUERY_TAG */, &wait,
                            4, &tag, 4, &got, 0)) {
            struct { uint32_t Tag, Level; int32_t AtRate; } q = {tag, 0, 0};
            struct {
                uint32_t Capabilities;
                uint8_t Technology, Rsvd[3], Chemistry[4];
                uint32_t DesignedCapacity, FullChargedCapacity;
                uint32_t DefaultAlert1, DefaultAlert2, CriticalBias, CycleCount;
            } bi;
            memset(&bi, 0, sizeof bi);
            if (DeviceIoControl(h, 0x294044 /* ..._QUERY_INFORMATION */, &q,
                                sizeof q, &bi, sizeof bi, &got, 0) &&
                !(bi.Capabilities & 0x40000000u /* relative, not mWh */)) {
                design = bi.DesignedCapacity;
                cycles = bi.CycleCount;
                memcpy(chem, bi.Chemistry, 4);
                have = true;
            }
        }
        CloseHandle(h);
    }
    out->design_mwh = design;
    out->cycles = cycles;
    memcpy(out->chem, chem, sizeof chem);
}

bool pfs_battery(struct pfs_batt *out) {
    CallNtPowerF f = power_fn();
    if (!f) return false;
    struct {
        uint8_t AcOnLine, BatteryPresent, Charging, Discharging;
        uint8_t Spare1[3], Tag;
        uint32_t MaxCapacity, RemainingCapacity;
        int32_t Rate; // signed milliwatts; negative while discharging
        uint32_t EstimatedTime, DefaultAlert1, DefaultAlert2;
    } b = {0};
    if (f(5 /* SystemBatteryState */, 0, 0, &b, sizeof b)) return false;
    out->present = b.BatteryPresent;
    out->ac = b.AcOnLine;
    out->charging = b.Charging;
    out->discharging = b.Discharging;
    out->max_mwh = b.MaxCapacity;
    out->rem_mwh = b.RemainingCapacity;
    out->rate_mw = b.Rate < 0 ? -b.Rate : b.Rate;
    out->design_mwh = 0;
    out->cycles = 0;
    memset(out->chem, 0, sizeof out->chem);
    if (out->present) batt_design(out);
    return true;
}

int pfs_cpu_mhz(uint32_t *cur, uint32_t *max, int cap) {
    CallNtPowerF f = power_fn();
    if (!f) return 0;
    struct {
        uint32_t Number, MaxMhz, CurrentMhz, MhzLimit, MaxIdleState,
            CurrentIdleState;
    } p[64] = {{0}};
    struct NtSystemInfo si;
    GetSystemInfo(&si);
    int n = (int)si.dwNumberOfProcessors;
    if (n < 1) n = 1;
    if (n > 64) n = 64;
    if (n > cap) n = cap;
    if (f(11 /* ProcessorInformation */, 0, 0, p, (uint32_t)(n * sizeof p[0])))
        return 0;
    for (int i = 0; i < n; i++) {
        cur[i] = p[i].CurrentMhz;
        max[i] = p[i].MaxMhz;
    }
    return n;
}

// The processor relationship records, walked twice. Core records (relation
// 0) number the cores and package records (relation 3) number the packages.
// The verified layout is {u32 relation, u32 size} per record, GroupCount 22
// bytes into the body, and the GROUP_AFFINITY array (16 bytes each, mask
// first, group number at +8) at 24. Only group 0 is read; everything here
// already caps at 64 logical cpus.
typedef int32_t (__msabi *GlpiExF)(uint32_t, void *, uint32_t *);

int pfs_cpu_topology(uint8_t *coreid, uint8_t *pkgid, int cap) {
    static GlpiExF f;
    if (!f)
        f = (GlpiExF)pfs_sym(u"kernel32.dll",
                             "GetLogicalProcessorInformationEx");
    if (!f) return 0;
    static char buf[65536]; // guarded by the carrier's lock
    uint32_t len = sizeof buf;
    if (!f(0xffff /* RelationAll */, buf, &len)) return 0;
    if (cap > 64) cap = 64;
    memset(coreid, 0, (size_t)cap);
    memset(pkgid, 0, (size_t)cap);
    int n = 0, ncore = 0, npkg = 0;
    for (int pass = 0; pass < 2; pass++) {
        uint32_t rel = pass ? 3 : 0; // cores first, then packages
        for (uint32_t off = 0; off + 32 <= len;) {
            uint32_t r = *(uint32_t *)(buf + off);
            uint32_t sz = *(uint32_t *)(buf + off + 4);
            if (!sz || off + sz > len) break;
            if (r == rel) {
                uint16_t groups = *(uint16_t *)(buf + off + 8 + 22);
                for (int g = 0; g < groups; g++) {
                    const char *ga = buf + off + 8 + 24 + (size_t)g * 16;
                    if (*(uint16_t *)(ga + 8) != 0) continue; // group 0 only
                    uint64_t mask = *(uint64_t *)ga;
                    for (int c = 0; c < cap; c++) {
                        if (!(mask >> c & 1)) continue;
                        if (pass) pkgid[c] = (uint8_t)npkg;
                        else coreid[c] = (uint8_t)ncore;
                        if (c + 1 > n) n = c + 1;
                    }
                }
                if (pass) npkg++;
                else ncore++;
            }
            off += sz;
        }
    }
    return ncore ? n : 0;
}
