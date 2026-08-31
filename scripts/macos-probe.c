#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/utsname.h>
#include <unistd.h>
#include <libc/dce.h>
#include <libc/runtime/syslib.internal.h>

#define XNU_CTL_KERN        1
#define XNU_KERN_ARGMAX     8
#define XNU_KERN_PROC       14
#define XNU_KERN_PROC_ALL   0
#define XNU_KERN_PROCARGS2  49
#define XNU_RTLD_NOW        2

#define XNU_PROC_ALL_PIDS         1
#define XNU_PROC_PIDLISTFDS       1
#define XNU_PROC_PIDTASKALLINFO   2
#define XNU_PROC_PIDTBSDINFO      3
#define XNU_PROC_PIDTASKINFO      4
#define XNU_PROC_PIDVNODEPATHINFO 9
#define XNU_PROC_PIDFDVNODEPATHINFO 2
#define XNU_RUSAGE_INFO_V2        2

typedef int listpids_f(uint32_t, uint32_t, void *, int);
typedef int pidinfo_f(int, int, uint64_t, void *, int);
typedef int pidpath_f(int, void *, uint32_t);
typedef int pidfdinfo_f(int, int, int, void *, int);
typedef int name_f(int, void *, uint32_t);
typedef int pid_rusage_f(int, int, void *);

static const char *g_outdir;
static FILE *g_manifest;

static void record(const char *name, const void *buf, long len,
                   const char *note) {
  char path[512];
  if (len >= 0 && buf) {
    snprintf(path, sizeof(path), "%s/%s.bin", g_outdir, name);
    FILE *f = fopen(path, "wb");
    if (f) {
      fwrite(buf, 1, len, f);
      fclose(f);
    }
  }
  fprintf(g_manifest, "%-32s %10ld  %s\n", name, len, note ? note : "");
  printf("%-32s %10ld  %s\n", name, len, note ? note : "");
}

// Size negotiation first, then the real fetch, like every sysctl consumer.
static void dump_byname(const char *name) {
  char file[128];
  size_t n = strlen(name);
  if (n >= sizeof(file)) n = sizeof(file) - 1;
  for (size_t i = 0; i < n; i++) file[i] = name[i] == '.' ? '_' : name[i];
  file[n] = 0;

  size_t len = 0;
  long rc = __syslib->__sysctlbyname(name, 0, &len, 0, 0);
  if (rc < 0) {
    record(file, 0, rc, name);
    return;
  }
  void *buf = malloc(len + 1);
  rc = __syslib->__sysctlbyname(name, buf, &len, 0, 0);
  record(file, buf, rc < 0 ? rc : (long)len, name);
  free(buf);
}

static void dump_mib(const char *file, int *mib, unsigned miblen,
                     const char *note) {
  size_t len = 0;
  long rc = __syslib->__sysctl(mib, miblen, 0, &len, 0, 0);
  if (rc < 0) {
    record(file, 0, rc, note);
    return;
  }
  // kern.proc.all can grow between the two calls, leave headroom.
  size_t cap = len + len / 4 + 4096;
  void *buf = malloc(cap);
  len = cap;
  rc = __syslib->__sysctl(mib, miblen, buf, &len, 0, 0);
  record(file, buf, rc < 0 ? rc : (long)len, note);
  free(buf);
}

int main(int argc, char *argv[]) {
  struct utsname u;
  uname(&u);
  printf("uname %s %s %s\n", u.sysname, u.release, u.machine);

  if (!IsXnuSilicon()) {
    printf("not Apple Silicon macOS, nothing to dump\n");
    return 1;
  }
  if (!__syslib) {
    printf("no Syslib table, loader too old or not the arm64 loader\n");
    return 1;
  }
  printf("syslib magic=%s version=%d (header %d, dlsym>=6, sysctl>=10)\n",
         __syslib->__magic == SYSLIB_MAGIC ? "ok" : "BAD",
         __syslib->__version, SYSLIB_VERSION);

  g_outdir = argc > 1 ? argv[1] : "fixtures";
  mkdir(g_outdir, 0755);
  char mpath[512];
  snprintf(mpath, sizeof(mpath), "%s/manifest.txt", g_outdir);
  g_manifest = fopen(mpath, "w");
  if (!g_manifest) {
    printf("cannot write %s\n", mpath);
    return 1;
  }
  fprintf(g_manifest, "# uname %s %s %s, syslib v%d\n", u.sysname, u.release,
          u.machine, __syslib->__version);

  if (__syslib->__version >= 10) {
    static const char *names[] = {
        "kern.ostype",     "kern.osrelease", "kern.osversion",
        "kern.osproductversion", "kern.version", "kern.hostname",
        "kern.boottime",   "kern.maxproc",   "kern.argmax",
        "hw.ncpu",         "hw.activecpu",   "hw.memsize",
        "hw.pagesize",     "hw.model",       "hw.tbfrequency",
        "machdep.cpu.brand_string", "vm.swapusage", "vm.loadavg",
    };
    for (size_t i = 0; i < sizeof(names) / sizeof(*names); i++)
      dump_byname(names[i]);

    int all[] = {XNU_CTL_KERN, XNU_KERN_PROC, XNU_KERN_PROC_ALL, 0};
    dump_mib("kern_proc_all", all, 4, "kinfo_proc array");
    int args[] = {XNU_CTL_KERN, XNU_KERN_PROCARGS2, getpid()};
    dump_mib("self_procargs2", args, 3, "KERN_PROCARGS2 for own pid");
  } else {
    printf("syslib v%d has no sysctl, skipping that section\n",
           __syslib->__version);
  }

  if (__syslib->__version >= 6) {
    void *h = __syslib->__dlopen("/usr/lib/libproc.dylib", XNU_RTLD_NOW);
    if (!h) {
      printf("dlopen libproc failed, %s\n", __syslib->__dlerror());
      fclose(g_manifest);
      return 1;
    }
    listpids_f *listpids = __syslib->__dlsym(h, "proc_listpids");
    pidinfo_f *pidinfo = __syslib->__dlsym(h, "proc_pidinfo");
    pidpath_f *pidpath = __syslib->__dlsym(h, "proc_pidpath");
    pidfdinfo_f *pidfdinfo = __syslib->__dlsym(h, "proc_pidfdinfo");
    name_f *procname = __syslib->__dlsym(h, "proc_name");
    pid_rusage_f *pidrusage = __syslib->__dlsym(h, "proc_pid_rusage");
    printf("libproc symbols listpids=%d pidinfo=%d pidpath=%d pidfdinfo=%d "
           "name=%d rusage=%d\n",
           !!listpids, !!pidinfo, !!pidpath, !!pidfdinfo, !!procname,
           !!pidrusage);

    int me = getpid();
    char buf[65536];
    if (listpids) {
      int need = listpids(XNU_PROC_ALL_PIDS, 0, 0, 0);
      char *big = malloc(need + 4096);
      int rc = listpids(XNU_PROC_ALL_PIDS, 0, big, need + 4096);
      record("proc_listpids", big, rc, "pid_t array, bytes");
      free(big);
    }
    if (pidpath) {
      int rc = pidpath(me, buf, sizeof(buf));
      record("self_pidpath", buf, rc, "proc_pidpath");
    }
    if (procname) {
      memset(buf, 0, 64);
      int rc = procname(me, buf, sizeof(buf));
      record("self_name", buf, rc > 0 ? rc : (long)strlen(buf), "proc_name");
    }
    if (pidinfo) {
      struct {
        const char *file;
        int flavor;
      } fl[] = {
          {"self_taskallinfo", XNU_PROC_PIDTASKALLINFO},
          {"self_tbsdinfo", XNU_PROC_PIDTBSDINFO},
          {"self_taskinfo", XNU_PROC_PIDTASKINFO},
          {"self_vnodepathinfo", XNU_PROC_PIDVNODEPATHINFO},
          {"self_listfds", XNU_PROC_PIDLISTFDS},
      };
      for (size_t i = 0; i < sizeof(fl) / sizeof(*fl); i++) {
        int rc = pidinfo(me, fl[i].flavor, 0, buf, sizeof(buf));
        record(fl[i].file, buf, rc, "proc_pidinfo, bytes filled");
      }
      // Cross pid access as a plain user, to learn what the runner allows.
      int rc = pidinfo(1, XNU_PROC_PIDTBSDINFO, 0, buf, sizeof(buf));
      record("pid1_tbsdinfo", buf, rc, "proc_pidinfo on launchd");
    }
    if (pidfdinfo) {
      int rc = pidfdinfo(me, 0, XNU_PROC_PIDFDVNODEPATHINFO, buf, sizeof(buf));
      record("self_fd0_vnodepathinfo", buf, rc, "proc_pidfdinfo on fd 0");
    }
    if (pidrusage) {
      memset(buf, 0, 1024);
      int rc = pidrusage(me, XNU_RUSAGE_INFO_V2, buf);
      record("self_rusage_v2", buf, rc == 0 ? 1024 : rc, "proc_pid_rusage");
    }
  } else {
    printf("syslib v%d has no dlsym, skipping libproc section\n",
           __syslib->__version);
  }

  fclose(g_manifest);
  printf("fixtures written to %s\n", g_outdir);
  return 0;
}
