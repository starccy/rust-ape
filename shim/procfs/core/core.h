// The carrier of the /proc emulation. It decides when generators run and
// where their text goes. Windows only; everywhere else every entry point
// declines at once, and Linux keeps its real /proc.
//
// Three access shapes exist and each gets its own answer:
//
//   1. Opening a content file for reading, whether open("/proc/123/stat")
//      or openat(dirfd, "stat") through a directory descriptor we handed
//      out, is intercepted in shim/open.c before the host sees it. The
//      generator runs now and its output stays in memory behind a
//      descriptor slot cosmo reserved for us; the shim's read, lseek,
//      fstat and close serve it (open.c). Fresh at every open, regenerated
//      in place by lseek(fd, 0, SEEK_SET) for readers that keep it open,
//      gone at close, never on disk. Directory descriptors of the tree are
//      remembered by fd (validated against cosmo's handle, forgotten by
//      shim/close-nt.c) so the relative spelling can be joined back to its
//      virtual path. A memory descriptor answers read, lseek, fstat, fcntl
//      flags and close; dup and pread are not served, no reader of /proc
//      does either. Listings of /proc, a process directory, its task/ and
//      net/ come from memory too (virtdir.c, through the vendored
//      shim/dirstream.c), so enumerating every thread of every process
//      costs no directory operation on disk.
//
//   2. Everything that walks or stats, opendir("/proc"), stat("/proc/1"),
//      access() and the like, lands in a materialized skeleton under
//      <TMP>/rust-ape-proc-<pid>/, reached through one prefix rewrite in
//      shim/mkntpath.c (and, for dirfd-relative names, the join in
//      shim/mkntpathat.c). Directories are real; content files are written
//      once per process on first access by name and then left alone: they
//      exist for stat() and for listings, and nobody reads them, since
//      every read is shape 1. Rewriting them per change would cost a write
//      plus the filter drivers' scan of a modified file on its next open
//      (~0.6ms each with Defender's real-time protection), which is what
//      made a monitor's sweep take seconds.
//
//   3. Links (exe, cwd, fd/<n>, self) are plain files holding the link
//      text, because NT can store a symlink but not open one the way
//      callers name it (O_PATH|O_NOFOLLOW is ELOOP there). The vendored
//      shim/readlinkat.c routes every /proc-shaped readlink here instead.
//
// The generators live in ../pid.c, ../net.c, ../sysinfo.c and ../sysctl.c
// and only ever emit into memory; swapping this carrier out does not touch
// them. The carrier itself is split by shape: tree.c holds the root, the
// lock and the path model, materialize.c the skeleton and the rewrite
// entry (shape 2), open.c the content and directory descriptors (shape 1),
// links.c the readlink answers (shape 3), sysfs.c the emulated slices of
// /sys. Everything here is private to this directory.

#ifndef RUST_APE_SHIM_PROCFS_CORE_H_
#define RUST_APE_SHIM_PROCFS_CORE_H_

#include <pthread.h>
#include <stdatomic.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#include "../procfs.h"

// The hosts this carrier serves: NT, and Apple Silicon macOS, where it
// stays entirely in memory (the disk skeleton and its path rewrites are
// NT-only). Every user includes libc/dce.h already.
#define PC_HOSTED() (IsWindows() || IsXnuSilicon())

// Cross-file entry points also used inside the carrier.
bool __ape_shim_procfs_join(int dirfd, const char *rel, char *out,
                            unsigned long outsz);
unsigned __ape_shim_procfs_link_mode(const char *vpath);
long __ape_shim_procfs_readlinkat(int dirfd, const char *path, char *buf,
                                  unsigned long bufsiz);

#define ROOT_LIST_MS 1000 // pid-directory sync under /proc
#define PID_DIR_MS 200    // one process's directory contents
#define NET_MS 250        // the materialized net/* files
#define STALE_SECS 3600   // trees whose owner died without cleanup

// tree.c. The skeleton root and the one lock every generation runs under.
extern char pc_root[512];
extern size_t pc_rootlen;
extern pthread_mutex_t pc_lock;
// Set by the thread that holds pc_lock while it generates content, so its own
// opens and writes re-enter mkntpath and are passed through as already real.
// Per thread, since another reader arriving meanwhile must wait for the lock
// and then be answered, not be mistaken for one of those writes.
extern _Thread_local int pc_busy;

void pc_write_file(const char *path, const char *data, size_t n);
void pc_rm_rf(const char *path);
bool pc_ensure_root(void);
uint64_t pc_fnv64(const void *data, size_t n, uint64_t h);

// Path model. `sub` is what follows "/proc" -- "" or "/...". Parsing also
// resolves the self alias, so every physical path it yields already names
// the real pid and openat through a dirfd of it just works.
enum kind {
    K_OTHER, // unknown: still mapped, found absent
    K_ROOT,
    K_TOP,      // /proc/<name>
    K_PID_DIR,  // /proc/<pid>
    K_PID_SUB,  // /proc/<pid>/... (name = first component, rest = deeper)
    K_NET_DIR,
    K_NET_FILE,
    K_SYS, // /proc/sys[/rest]
};

struct node {
    enum kind kind;
    bool was_self; // the pid came from the "self" alias
    uint32_t pid;
    char name[64];
    char rest[200];
};

void pc_parse(const char *sub, struct node *n);
// The physical suffix under pc_root for a parsed path.
void pc_phys_suffix(const struct node *n, const char *sub, char *out,
                    size_t cap);

// materialize.c. Bringing the on-disk skeleton up to date, one ensure per
// path shape, and the content of one /proc/<pid> file out of its slot.
void pc_ensure_node(const struct node *n);
void pc_ensure_pid(uint32_t pid, const char *name);
void __ape_shim_procfs_list(const char *vpath);
bool pc_pid_content(uint32_t pid, const char *name, struct pfs_buf *out);
int64_t pc_content_gen(const char *vpath);

// sysfs.c. The emulated slices of /sys; the rest of /sys stays alone.
struct pc_sysfs {
    const char *prefix;
    int len;
    void (*ensure)(void);
    const char *dir;
};
const struct pc_sysfs *pc_sysfs_match(const char *path);
bool pc_gen_sysfs(const char *path, struct pfs_buf *out);

// open.c. Descriptors of the tree, content and directory alike, remembered
// by fd under their virtual path.
struct trackfd {
    _Atomic(int) fd; // -1: free
    long handle;
    char vpath[200];
};
struct trackfd *pc_track_find(int fd);
struct trackfd *pc_track_get(int fd);
bool pc_gen_node(const struct node *n, struct pfs_buf *b);

// links.c. The link a parsed path names, exe/cwd/root of a process or of
// one of its threads, or 0.
const char *pc_link_name(const struct node *n);

#endif
