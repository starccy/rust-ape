// [rust-ape] Windows subprocess management, replacing cosmo 4.0.2's
// libc.a(proc.o) so a native win32 child's exit code is not misread as
// a cosmo wait status.
//
// cosmo's convention on NT is that the 32-bit exit code carries a whole
// unix wait status: a cosmo child that calls exit(N) really exits with
// N<<8, one that dies of a signal exits with the raw signal number, and
// __proc_harvest() stores the code verbatim. A native win32 program
// knows nothing of this and exits with a literal code, which the
// verbatim store then misreads: every nonzero exit of every native
// program becomes a death by the signal of that number.
//
// Both spawn paths funnel here. posix_spawn() tracks the child handle
// directly, and the fork+execve path relays it: execve() on NT spawns
// the real process, hands its duplicated handle back through the
// 0x23000000 exit-code channel, and __proc_harvest() swaps it in. So by
// the time an exit code is being decoded, pr->handle is the native
// process itself, and that is the one reliable moment to ask what kind
// of program it was: QueryFullProcessImageNameW() names the image, and
// its first bytes decide -- an APE starts with "MZqFpD", a native
// program is any other PE. Native codes are then re-encoded with unix
// semantics: the NTSTATUS crash codes become the corresponding signal
// (the same table cosmopolitan master added to wait4-nt.c for cosmo's
// own crashes), and everything else is an ordinary exit, (code&0xFF)<<8.
// A cosmo child, a script, or an unreadable image keeps the upstream
// decode bit for bit.
//
// The sniff runs once per child exit, on the open handle cosmo already
// holds; a query that fails just means upstream behavior. The rest is a
// faithful copy of upstream. Compiled with -D_COSMO_SOURCE by the
// linker wrapper. Revisit on toolchain upgrade.
//
// Derived from cosmopolitan libc/proc/proc.c,
// Copyright 2023 Justine Alexandra Roberts Tunney, ISC license.

#include <stdbool.h>
/*-*- mode:c;indent-tabs-mode:nil;c-basic-offset:2;tab-width:8;coding:utf-8 -*-│
│ vi: set et ft=c ts=2 sts=2 sw=2 fenc=utf-8                               :vi │
╞══════════════════════════════════════════════════════════════════════════════╡
│ Copyright 2023 Justine Alexandra Roberts Tunney                              │
│                                                                              │
│ Permission to use, copy, modify, and/or distribute this software for         │
│ any purpose with or without fee is hereby granted, provided that the         │
│ above copyright notice and this permission notice appear in all copies.      │
│                                                                              │
│ THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL                │
│ WARRANTIES WITH REGARD TO THIS SOFTWARE INCLUDING ALL IMPLIED                │
│ WARRANTIES OF MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE             │
│ AUTHOR BE LIABLE FOR ANY SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL         │
│ DAMAGES OR ANY DAMAGES WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR        │
│ PROFITS, WHETHER IN AN ACTION OF CONTRACT, NEGLIGENCE OR OTHER               │
│ TORTIOUS ACTION, ARISING OUT OF OR IN CONNECTION WITH THE USE OR             │
│ PERFORMANCE OF THIS SOFTWARE.                                                │
╚─────────────────────────────────────────────────────────────────────────────*/
#include "libc/proc/proc.h"
#include "libc/calls/calls.h"
#include "libc/calls/internal.h"
#include "libc/calls/sig.internal.h"
#include "libc/calls/state.internal.h"
#include "libc/calls/struct/rusage.h"
#include "libc/calls/struct/siginfo.h"
#include "libc/calls/struct/sigset.internal.h"
#include "libc/calls/syscall_support-nt.internal.h"
#include "libc/cosmo.h"
#include "libc/errno.h"
#include "libc/fmt/wintime.internal.h"
#include "libc/intrin/dll.h"
#include "libc/intrin/maps.h"
#include "libc/intrin/strace.h"
#include "libc/intrin/weaken.h"
#include "libc/mem/leaks.h"
#include "libc/nt/accounting.h"
#include "libc/nt/enum/heap.h"
#include "libc/nt/enum/processaccess.h"
#include "libc/nt/enum/processcreationflags.h"
#include "libc/nt/enum/status.h"
#include "libc/nt/enum/wait.h"
#include "libc/nt/events.h"
#include "libc/nt/memory.h"
#include "libc/nt/process.h"
#include "libc/nt/runtime.h"
#include "libc/nt/struct/filetime.h"
#include "libc/nt/struct/iocounters.h"
#include "libc/nt/struct/processmemorycounters.h"
#include "libc/nt/synchronization.h"
#include "libc/nt/thread.h"
#include "libc/runtime/runtime.h"
#include "libc/str/str.h"
#include "libc/sysv/consts/map.h"
#include "libc/sysv/consts/prot.h"
#include "libc/sysv/consts/sa.h"
#include "libc/sysv/consts/sicode.h"
#include "libc/sysv/consts/sig.h"
#include "libc/sysv/errfuns.h"
#include "libc/thread/thread.h"
#include "libc/thread/tls.h"
#include "third_party/nsync/mu.h"
// [rust-ape] for the native-child exit-code sniff in __proc_harvest
#include "libc/nt/createfile.h"
#include "libc/nt/dll.h"
#include "libc/nt/enum/creationdisposition.h"
#include "libc/nt/enum/fileflagandattributes.h"
#include "libc/nt/enum/filesharemode.h"
#include "libc/nt/thunk/msabi.h"
#ifdef __x86_64__

/**
 * @fileoverview Windows Subprocess Management.
 */

#define STACK_SIZE 65536

struct Procs __proc;
static pthread_mutex_t __proc_lock_obj = PTHREAD_MUTEX_INITIALIZER;

textwindows static void __proc_stats(int64_t h, struct rusage *ru) {
  bzero(ru, sizeof(*ru));
  struct NtProcessMemoryCountersEx memcount = {sizeof(memcount)};
  GetProcessMemoryInfo(h, &memcount, sizeof(memcount));
  ru->ru_maxrss = memcount.PeakWorkingSetSize / 1024;
  ru->ru_majflt = memcount.PageFaultCount;
  struct NtFileTime createtime, exittime;
  struct NtFileTime kerneltime, usertime;
  GetProcessTimes(h, &createtime, &exittime, &kerneltime, &usertime);
  ru->ru_utime = WindowsDurationToTimeVal(ReadFileTime(usertime));
  ru->ru_stime = WindowsDurationToTimeVal(ReadFileTime(kerneltime));
  struct NtIoCounters iocount;
  GetProcessIoCounters(h, &iocount);
  ru->ru_inblock = iocount.ReadOperationCount;
  ru->ru_oublock = iocount.WriteOperationCount;
}

// [rust-ape] Whether the process behind this handle is a native win32
// program rather than a cosmo one, decided by the first bytes of its
// image file: every APE starts with "MZqFpD". False on any failure, so
// the caller falls back to the upstream decode.
//
// K32GetProcessImageFileNameW, not QueryFullProcessImageNameW: by the
// time __proc_harvest() runs the child is dead, and the Query call
// fails on a terminated process with ERROR_GEN_FAILURE. The K32 call
// answers from the kernel process object and keeps working after death,
// but names the image by its NT device path
// (\Device\HarddiskVolume3\...\cmd.exe), which CreateFile only accepts
// behind the \\?\GLOBALROOT prefix.
textwindows static bool __proc_is_native_child(int64_t hProcess) {
  typedef uint32_t (__msabi *GetImageF)(int64_t, char16_t *, uint32_t);
  static GetImageF get_image;
  static bool sought;
  if (!sought) {
    // not in cosmo's import tables, hence the runtime lookup
    get_image = (GetImageF)GetProcAddress(GetModuleHandle("kernel32.dll"),
                                          "K32GetProcessImageFileNameW");
    sought = true;
  }
  if (!get_image)
    return false;
  char16_t dev[1024];
  uint32_t len = get_image(hProcess, dev, 1024);
  if (!len || len >= 1024)
    return false;
  char16_t path[1024 + 16] = u"\\\\?\\GLOBALROOT";
  for (uint32_t i = 0; i <= len; i++) path[14 + i] = dev[i];
  int64_t h = CreateFile(path, kNtGenericRead,
                         kNtFileShareRead | kNtFileShareWrite |
                             kNtFileShareDelete,
                         0, kNtOpenExisting, kNtFileAttributeNormal, 0);
  if (h == kNtInvalidHandleValue)
    return false;
  char buf[8] = {0};
  uint32_t got = 0;
  bool32 ok = ReadFile(h, buf, 8, &got, 0);
  CloseHandle(h);
  if (!ok || got < 8)
    return false;
  return buf[0] == 'M' && buf[1] == 'Z' && memcmp(buf, "MZqFpD", 6) != 0;
}

// [rust-ape] A native child's exit code, re-encoded as a wait status.
// The NTSTATUS crash codes map to the signal a unix kernel would have
// delivered (the same table cosmopolitan master uses in wait4-nt.c);
// any other value is an ordinary exit, truncated the way POSIX does.
textwindows static uint32_t __proc_native_wstatus(uint32_t code) {
  switch (code) {
    case kNtStatusControlCExit:
      return SIGINT;
    case kNtStatusStackOverflow:
    case kNtStatusAccessViolation:
    case kNtStatusGuardPageViolation:
      return SIGSEGV;
    case kNtStatusInPageError:
      return SIGBUS;
    case kNtStatusIllegalInstruction:
    case kNtStatusPrivilegedInstruction:
      return SIGILL;
    case kNtStatusBreakpoint:
      return SIGTRAP;
    case kNtStatusIntegerOverflow:
    case kNtStatusFloatDivideByZero:
    case kNtStatusFloatOverflow:
    case kNtStatusFloatUnderflow:
    case kNtStatusFloatInexactResult:
    case kNtStatusFloatDenormalOperand:
    case kNtStatusFloatInvalidOperation:
    case kNtStatusFloatStackCheck:
    case kNtStatusIntegerDivideBYZero:
      return SIGFPE;
    case kNtStatusDllNotFound:
    case kNtStatusDllInitFailed:
    case kNtStatusOrdinalNotFound:
    case kNtStatusEntrypointNotFound:
      return SIGSYS;
    case kNtStatusAssertionFailure:
      return SIGABRT;
    default:
      return (code & 0xFF) << 8;
  }
}

// performs accounting on exited process
// multiple threads can wait on a process
// it's important that only one calls this
textwindows int __proc_harvest(struct Proc *pr, bool iswait4) {
  int sic = 0;
  uint32_t status;
  struct rusage ru;
  GetExitCodeProcess(pr->handle, &status);
  if (status == kNtStillActive)
    return 0;
  __proc_stats(pr->handle, &ru);
  rusage_add(&pr->ru, &ru);
  rusage_add(&__proc.ruchlds, &ru);
  if ((status & 0xFF000000u) == 0x23000000u) {
    // handle child execve()
    CloseHandle(pr->handle);
    pr->handle = status & 0x00FFFFFF;
  } else {
    // handle child _Exit()
    if (status == 0xc9af3d51u)
      status = kNtStillActive;
    else if (__proc_is_native_child(pr->handle))
      // [rust-ape] a native program's exit code is not a wait status
      status = __proc_native_wstatus(status);
    pr->wstatus = status;
    if (!iswait4 && !pr->waiters && !__proc.waiters &&
        (__sighandrvas[SIGCHLD] == (uintptr_t)SIG_IGN ||
         (__sighandflags[SIGCHLD] & SA_NOCLDWAIT))) {
      // perform automatic zombie reaping
      STRACE("automatically reaping zombie");
      dll_remove(&__proc.list, &pr->elem);
      dll_make_first(&__proc.free, &pr->elem);
      CloseHandle(pr->handle);
    } else {
      // transitions process to zombie state
      // wait4 is responsible for reaping it
      pr->status = PROC_ZOMBIE;
      dll_remove(&__proc.list, &pr->elem);
      dll_make_first(&__proc.zombies, &pr->elem);
      SetEvent(__proc.haszombies);
      if (!pr->waiters && !__proc.waiters) {
        if (WIFSIGNALED(status)) {
          sic = CLD_KILLED;
        } else {
          sic = CLD_EXITED;
        }
      }
    }
  }
  return sic;
}

textwindows dontinstrument static uint32_t __proc_worker(void *arg) {
  struct CosmoTib tls;
  char *sp = __builtin_frame_address(0);
  __bootstrap_tls(&tls, __builtin_frame_address(0));
  __maps_track(
      (char *)(((uintptr_t)sp + __pagesize - 1) & -__pagesize) - STACK_SIZE,
      STACK_SIZE, PROT_READ | PROT_WRITE,
      MAP_PRIVATE | MAP_ANONYMOUS | MAP_NOFORK);
  for (;;) {

    // assemble a group of processes to wait on. if more than 64
    // children exist, then we'll use a small timeout and select
    // processes with a shifting window via a double linked list
    // if fewer than 64 processes exist then we'll also wait for
    // process birth notifications, and wait on them immediately
    int64_t handles[64];
    struct Proc *objects[64];
    uint32_t millis, i, n = 0;
    struct Dll *e, *e2, *samples = 0;
    __proc_lock();
    for (e = dll_first(__proc.list); e && n < 64; e = e2) {
      struct Proc *pr = PROC_CONTAINER(e);
      e2 = dll_next(__proc.list, e);
      // cycle process to end of list
      dll_remove(&__proc.list, e);
      dll_make_last(&samples, e);
      // don't bother waiting if it's already awaited
      if (!pr->waiters) {
        handles[n] = pr->handle;
        objects[n] = pr;
        ++pr->waiters;
        ++n;
      }
    }
    dll_make_last(&__proc.list, samples);
    __proc_unlock();

    // wait for something to happen
    if (n == 64) {
      millis = POLL_INTERVAL_MS;
    } else {
      millis = -1u;
      handles[n++] = __proc.onbirth;
    }
    i = WaitForMultipleObjects(n, handles, false, millis);
    if (i == -1u) {
      STRACE("proc wait panic %d", GetLastError());
      _Exit(157);
    }
    if (i & kNtWaitAbandoned) {
      i &= ~kNtWaitAbandoned;
      STRACE("proc %u handle %ld abandoned", i, handles[i]);
    }
    __proc_lock();

    // release our waiter status
    for (int j = 0; j < n; ++j) {
      if (handles[j] == __proc.onbirth)
        continue;
      if (j == i)
        continue;
      if (!--objects[j]->waiters && objects[j]->status == PROC_UNDEAD)
        __proc_free(objects[j]);
    }

    // check if we need to churn due to >64 processes
    if (i == kNtWaitTimeout) {
      __proc_unlock();
      continue;
    }

    // churn on new process birth
    if (handles[i] == __proc.onbirth) {
      __proc_unlock();
      continue;
    }

    // handle process status change
    int sic = 0;
    --objects[i]->waiters;
    switch (objects[i]->status) {
      case PROC_ALIVE:
        sic = __proc_harvest(objects[i], false);
        break;
      case PROC_ZOMBIE:
        break;
      case PROC_UNDEAD:
        if (!objects[i]->waiters)
          __proc_free(objects[i]);
        break;
      default:
        __builtin_unreachable();
    }

    __proc_unlock();

    // don't raise SIGCHLD if
    // 1. wait4() is being used
    // 2. SIGCHLD has SIG_IGN handler
    // 3. SIGCHLD has SA_NOCLDWAIT flag
    if (sic)
      __sig_generate(SIGCHLD, sic);
  }
  return 0;
}

/**
 * Lazy initializes process tracker data structures and worker.
 */
textwindows static void __proc_setup(void) {
  __proc.onbirth = CreateEvent(0, 0, 0, 0);     // auto reset
  __proc.haszombies = CreateEvent(0, 1, 0, 0);  // manual reset
  __proc.thread = CreateThread(0, STACK_SIZE, __proc_worker, 0,
                               kNtStackSizeParamIsAReservation, 0);
}

/**
 * Locks process tracker.
 */
textwindows void __proc_lock(void) {
  cosmo_once(&__proc.once, __proc_setup);
  _pthread_mutex_lock(&__proc_lock_obj);
}

/**
 * Unlocks process tracker.
 */
textwindows void __proc_unlock(void) {
  _pthread_mutex_unlock(&__proc_lock_obj);
}

/**
 * Resets process tracker from forked child.
 */
textwindows void __proc_wipe_and_reset(void) {
  // TODO(jart): Should we preserve this state in forked children?
  _pthread_mutex_wipe_np(&__proc_lock_obj);
  bzero(&__proc, sizeof(__proc));
}

/**
 * Allocates object for new process.
 *
 * The returned memory is not tracked by any list. It must be filled in
 * with system process information and then added back to the system by
 * calling __proc_add(). If process creation fails, then it needs to be
 * added back to the __proc.free list by caller.
 */
textwindows struct Proc *__proc_new(void) {
  struct Dll *e;
  struct Proc *proc = 0;
  if ((e = dll_first(__proc.free))) {
    proc = PROC_CONTAINER(e);
    dll_remove(&__proc.free, &proc->elem);
  }
  if (!proc && !(proc = HeapAlloc(GetProcessHeap(), 0, sizeof(struct Proc))))
    return 0;
  bzero(proc, sizeof(*proc));
  dll_init(&proc->elem);
  return proc;
}

/**
 * Adds process to active list.
 *
 * The handle and pid must be filled in before calling this.
 */
textwindows void __proc_add(struct Proc *proc) {
  dll_make_first(&__proc.list, &proc->elem);
  SetEvent(__proc.onbirth);
}

textwindows void __proc_free(struct Proc *pr) {
  dll_remove(&__proc.undead, &pr->elem);
  dll_make_first(&__proc.free, &pr->elem);
  CloseHandle(pr->handle);
}

// returns owned handle of direct child process
// this is intended for the __proc_handle() implementation
textwindows int64_t __proc_search(int pid) {
  struct Dll *e;
  int64_t handle = 0;
  BLOCK_SIGNALS;
  __proc_lock();
  // TODO(jart): we should increment a reference count when returning
  for (e = dll_first(__proc.list); e; e = dll_next(__proc.list, e)) {
    if (pid == PROC_CONTAINER(e)->pid) {
      handle = PROC_CONTAINER(e)->handle;
      break;
    }
  }
  __proc_unlock();
  ALLOW_SIGNALS;
  return handle;
}

#endif /* __x86_64__ */
