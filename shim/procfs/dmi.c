// The DMI/SMBIOS facts behind /sys/class/dmi/id (and its real home,
// /sys/devices/virtual/dmi/id): vendor, product, board, chassis and BIOS
// identity. NT hands out the raw SMBIOS table through GetSystemFirmwareTable
// ('RSMB'), readable by any user; a probe confirmed the 8-byte header and
// the structure stream on this vintage. The table never changes while the
// machine is up, so it is parsed once.
//
// Serial numbers and the system UUID are deliberately left out: Linux
// keeps those root-only, and a world-readable file here would hand them to
// every process.

#define _COSMO_SOURCE // for libc/dce.h's IsWindows()

#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <libc/dce.h>
#include <libc/nt/thunk/msabi.h>

#include "procfs.h"

typedef uint32_t (__msabi *GsftF)(uint32_t, uint32_t, void *, uint32_t);

// The idx'th string of a structure's string table (1-based), trailing
// blanks dropped; empty when absent.
static void take(char *out, size_t cap, const uint8_t *s, const uint8_t *end,
                 int idx) {
    out[0] = 0;
    if (idx <= 0) return;
    for (int i = 1; s < end && *s; i++) {
        if (i == idx) {
            size_t n = 0;
            while (s + n < end && s[n] && n < cap - 1) n++;
            memcpy(out, s, n);
            while (n && out[n - 1] == ' ') n--;
            out[n] = 0;
            return;
        }
        while (s < end && *s) s++;
        s++;
    }
}

bool pfs_dmi(struct pfs_dmi *out) {
    static struct pfs_dmi cached;
    static int state; // 0 untried, 1 have, -1 failed
    if (state == 0) {
        state = -1;
        GsftF f = (GsftF)pfs_sym(u"kernel32.dll", "GetSystemFirmwareTable");
        if (f) {
            uint32_t need = f(0x52534D42 /* 'RSMB' */, 0, 0, 0);
            uint8_t *buf = need ? malloc(need) : 0;
            uint32_t got = buf ? f(0x52534D42, 0, buf, need) : 0;
            if (got > 8) {
                memset(&cached, 0, sizeof cached);
                const uint8_t *p = buf + 8, *end = buf + got;
                while (p + 4 <= end) {
                    uint8_t type = p[0], len = p[1];
                    if (len < 4) break;
                    const uint8_t *strs = p + len;
                    const uint8_t *q = strs;
                    while (q + 1 < end && !(q[0] == 0 && q[1] == 0)) q++;
                    const uint8_t *next = q + 2;
                    if (type == 127) break;
#define S(field, i) take(cached.field, sizeof cached.field, strs, end, (i))
                    if (type == 0 && len >= 9) {
                        cached.have_bios = true;
                        S(bios_vendor, p[4]);
                        S(bios_version, p[5]);
                        S(bios_date, p[8]);
                    } else if (type == 1 && len >= 8) {
                        cached.have_sys = true;
                        S(sys_vendor, p[4]);
                        S(product_name, p[5]);
                        S(product_version, p[6]);
                        if (len > 0x1a) {
                            S(product_sku, p[0x19]);
                            S(product_family, p[0x1a]);
                        }
                    } else if (type == 2 && len >= 8) {
                        cached.have_board = true;
                        S(board_vendor, p[4]);
                        S(board_name, p[5]);
                        S(board_version, p[6]);
                        if (len > 8) S(board_asset_tag, p[8]);
                    } else if (type == 3 && len >= 7) {
                        cached.have_chassis = true;
                        S(chassis_vendor, p[4]);
                        cached.chassis_type = p[5] & 0x7f;
                        S(chassis_version, p[6]);
                    }
#undef S
                    p = next;
                }
                state = 1;
            }
            free(buf);
        }
    }
    if (state != 1) return false;
    *out = cached;
    return true;
}
