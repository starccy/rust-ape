// if_nametoindex/if_indextoname/if_nameindex/if_freenameindex, which cosmo
// does not have. An interface has both a name and a small integer index, and
// the index is what an IPv6 scope id or a multicast sockopt carries.
//
// Linux resolves both directions with SIOCGIFINDEX/SIOCGIFNAME. NT answers
// those ioctls with WSAEOPNOTSUPP and keeps the index in GetAdaptersAddresses
// instead; anything else falls back to enumeration order. All three calls
// share one enumeration, so name -> index -> name round-trips whichever path
// answered.

#define _COSMO_SOURCE // for libc/dce.h's IsLinux()/IsWindows()

#include <dirent.h>
#include <errno.h>
#include <stddef.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/socket.h>
#include <unistd.h>
#include <libc/dce.h>
#include <libc/nt/iphlpapi.h>
#include <libc/nt/struct/ipadapteraddresses.h>
#include <libc/sock/struct/ifconf.h>
#include <libc/sock/struct/ifreq.h>
#include <libc/sysv/consts/sio.h>

// musl's layout, which is what the Rust side has baked in.
struct if_nameindex {
    unsigned int if_index;
    char *if_name;
};

#define MAX_IFS 128

typedef char ifname_t[IFNAMSIZ];

static int if_socket(void) {
    return socket(AF_INET, SOCK_DGRAM, 0);
}

static int add_name(ifname_t *out, int n, int max, const char *name) {
    if (n >= max || !name[0] || strlen(name) >= IFNAMSIZ) return n;
    // "<name>:<n>" names one address of an interface; the interface is the
    // part in front, and carrying several addresses must not list it twice
    ifname_t base;
    size_t len = strcspn(name, ":");
    if (!len) return n;
    memcpy(base, name, len);
    base[len] = 0;
    // SIOCGIFCONF also repeats an interface once per address it carries
    for (int i = 0; i < n; i++)
        if (!strcmp(out[i], base)) return n;
    strcpy(out[n], base);
    return n + 1;
}

static int list_from_sysfs(ifname_t *out, int max) {
    DIR *d = opendir("/sys/class/net");
    if (!d) return -1;
    int n = 0;
    for (struct dirent *e; (e = readdir(d));)
        if (e->d_name[0] != '.') n = add_name(out, n, max, e->d_name);
    closedir(d);
    return n ? n : -1;
}

static int list_from_ioctl(ifname_t *out, int max) {
    int fd = if_socket();
    if (fd == -1) return -1;
    size_t bytes = MAX_IFS * sizeof(struct ifreq);
    char *buf = malloc(bytes);
    if (!buf) return close(fd), -1;
    struct ifconf ifc;
    ifc.ifc_len = (int32_t)bytes;
    ifc.ifc_buf = buf;
    int n = -1;
    if (ioctl(fd, SIOCGIFCONF, &ifc) != -1) {
        int cnt = (int)(ifc.ifc_len / (int32_t)sizeof(struct ifreq));
        n = 0;
        for (int i = 0; i < cnt; i++) {
            char name[IFNAMSIZ];
            memcpy(name, ifc.ifc_req[i].ifr_name, IFNAMSIZ - 1);
            name[IFNAMSIZ - 1] = 0;
            n = add_name(out, n, max, name);
        }
    }
    free(buf);
    close(fd);
    return n;
}

static int list_names(ifname_t *out, int max) {
    // /sys/class/net lists interfaces SIOCGIFCONF cannot see, since that is
    // an IPv4 address list, but it is not always mounted.
    if (IsLinux()) {
        int n = list_from_sysfs(out, max);
        if (n > 0) return n;
    }
    return list_from_ioctl(out, max);
}

// Cosmo names an NT interface after the adapter's FriendlyName with the
// non-name characters replaced and the tail cut to IFNAMSIZ-2, so both sides
// are folded into that shape and a prefix is accepted.
static char norm(unsigned c) {
    if ((c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') ||
        (c >= '0' && c <= '9') || c == '-' || c == '.')
        return (char)c;
    return '_';
}

static int name_matches(const char *name, const uint16_t *friendly, int exact) {
    if (!friendly) return 0;
    int i = 0;
    for (; name[i] && name[i] != ':'; i++) { // ':' begins an address suffix
        if (!friendly[i]) return 0;
        if (norm((unsigned char)name[i]) != norm(friendly[i])) return 0;
    }
    if (!i) return 0;
    return exact ? !friendly[i] : 1;
}

static unsigned nt_index_for(const char *name) {
    uint32_t flags = kNtGaaFlagSkipUnicast | kNtGaaFlagSkipAnycast |
                     kNtGaaFlagSkipMulticast | kNtGaaFlagSkipDnsServer;
    uint32_t size = 0;
    // sizing call: reports ERROR_BUFFER_OVERFLOW and fills in `size`
    GetAdaptersAddresses(0 /* AF_UNSPEC */, flags, 0, 0, &size);
    if (!size) return 0;
    struct NtIpAdapterAddresses *aa = malloc(size);
    if (!aa) return 0;
    unsigned idx = 0;
    if (GetAdaptersAddresses(0, flags, 0, aa, &size) == 0)
        // whole name first, so "Ethernet" cannot take "Ethernet 2"'s index
        for (int exact = 1; exact >= 0 && !idx; exact--)
            for (struct NtIpAdapterAddresses *p = aa; p && !idx; p = p->Next)
                if (name_matches(name, p->FriendlyName, exact)) idx = p->IfIndex;
    free(aa);
    return idx;
}

// Last resort: number the interfaces in the order the host lists them.
static unsigned synth_index(const char *name) {
    ifname_t names[MAX_IFS];
    int n = list_names(names, MAX_IFS);
    for (int i = 0; i < n; i++)
        if (!strcmp(names[i], name)) return (unsigned)(i + 1);
    return 0;
}

unsigned int if_nametoindex(const char *name) {
    if (!name || !name[0] || strlen(name) >= IFNAMSIZ) return errno = ENODEV, 0;

    unsigned idx = 0;
    int fd = if_socket();
    if (fd != -1) {
        struct ifreq r;
        memset(&r, 0, sizeof(r));
        strcpy(r.ifr_name, name);
        if (ioctl(fd, SIOCGIFINDEX, &r) != -1 && r.ifr_ifindex > 0)
            idx = (unsigned)r.ifr_ifindex;
        close(fd);
    }
    if (!idx && IsWindows()) idx = nt_index_for(name);
    if (!idx) idx = synth_index(name);
    if (!idx) errno = ENODEV;
    return idx;
}

char *if_indextoname(unsigned int index, char *name) {
    if (!index || !name) return errno = ENXIO, NULL;

    int fd = if_socket();
    if (fd != -1) {
        struct ifreq r;
        memset(&r, 0, sizeof(r));
        r.ifr_ifindex = (int)index;
        int ok = ioctl(fd, SIOCGIFNAME, &r) != -1 && r.ifr_name[0];
        close(fd);
        if (ok) {
            memcpy(name, r.ifr_name, IFNAMSIZ - 1);
            name[IFNAMSIZ - 1] = 0;
            return name;
        }
    }

    // Without the ioctl, invert if_nametoindex over the enumeration.
    ifname_t names[MAX_IFS];
    int n = list_names(names, MAX_IFS);
    for (int i = 0; i < n; i++)
        if (if_nametoindex(names[i]) == index) return strcpy(name, names[i]);

    return errno = ENXIO, NULL;
}

struct if_nameindex *if_nameindex(void) {
    ifname_t names[MAX_IFS];
    int n = list_names(names, MAX_IFS);
    if (n < 0) return errno = ENOBUFS, NULL;

    // One allocation holds the table, its zero terminator and the strings it
    // points at, so if_freenameindex is a single free().
    size_t bytes = (size_t)(n + 1) * sizeof(struct if_nameindex) +
                   (size_t)n * IFNAMSIZ;
    struct if_nameindex *t = calloc(1, bytes);
    if (!t) return errno = ENOBUFS, NULL;

    char *strings = (char *)(t + n + 1);
    for (int i = 0; i < n; i++) {
        t[i].if_index = if_nametoindex(names[i]);
        t[i].if_name = strings + (size_t)i * IFNAMSIZ;
        strcpy(t[i].if_name, names[i]);
    }
    return t;
}

void if_freenameindex(struct if_nameindex *p) {
    free(p);
}
