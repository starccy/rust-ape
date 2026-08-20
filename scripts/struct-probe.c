// Pulls every public struct/union/typedef cosmo declares into one object
// file's debug info, for scripts/struct-scan.py to read.

#define _GNU_SOURCE

#if __has_include(<aio.h>)
#include <aio.h>
#endif
#if __has_include(<arpa/inet.h>)
#include <arpa/inet.h>
#endif
#if __has_include(<dirent.h>)
#include <dirent.h>
#endif
#if __has_include(<fcntl.h>)
#include <fcntl.h>
#endif
#if __has_include(<fnmatch.h>)
#include <fnmatch.h>
#endif
#if __has_include(<glob.h>)
#include <glob.h>
#endif
#if __has_include(<grp.h>)
#include <grp.h>
#endif
#if __has_include(<ifaddrs.h>)
#include <ifaddrs.h>
#endif
#if __has_include(<langinfo.h>)
#include <langinfo.h>
#endif
#if __has_include(<link.h>)
#include <link.h>
#endif
#if __has_include(<locale.h>)
#include <locale.h>
#endif
#if __has_include(<mntent.h>)
#include <mntent.h>
#endif
#if __has_include(<net/if.h>)
#include <net/if.h>
#endif
#if __has_include(<net/route.h>)
#include <net/route.h>
#endif
#if __has_include(<netdb.h>)
#include <netdb.h>
#endif
#if __has_include(<netinet/in.h>)
#include <netinet/in.h>
#endif
#if __has_include(<netinet/ip.h>)
#include <netinet/ip.h>
#endif
#if __has_include(<netinet/tcp.h>)
#include <netinet/tcp.h>
#endif
#if __has_include(<netinet/udp.h>)
#include <netinet/udp.h>
#endif
#if __has_include(<poll.h>)
#include <poll.h>
#endif
#if __has_include(<pthread.h>)
#include <pthread.h>
#endif
#if __has_include(<pwd.h>)
#include <pwd.h>
#endif
#if __has_include(<regex.h>)
#include <regex.h>
#endif
#if __has_include(<sched.h>)
#include <sched.h>
#endif
#if __has_include(<semaphore.h>)
#include <semaphore.h>
#endif
#if __has_include(<setjmp.h>)
#include <setjmp.h>
#endif
#if __has_include(<signal.h>)
#include <signal.h>
#endif
#if __has_include(<spawn.h>)
#include <spawn.h>
#endif
#if __has_include(<stdio.h>)
#include <stdio.h>
#endif
#if __has_include(<sys/epoll.h>)
#include <sys/epoll.h>
#endif
#if __has_include(<sys/eventfd.h>)
#include <sys/eventfd.h>
#endif
#if __has_include(<sys/file.h>)
#include <sys/file.h>
#endif
#if __has_include(<sys/ioctl.h>)
#include <sys/ioctl.h>
#endif
#if __has_include(<sys/ipc.h>)
#include <sys/ipc.h>
#endif
#if __has_include(<sys/mman.h>)
#include <sys/mman.h>
#endif
#if __has_include(<sys/mount.h>)
#include <sys/mount.h>
#endif
#if __has_include(<sys/msg.h>)
#include <sys/msg.h>
#endif
#if __has_include(<sys/random.h>)
#include <sys/random.h>
#endif
#if __has_include(<sys/resource.h>)
#include <sys/resource.h>
#endif
#if __has_include(<sys/select.h>)
#include <sys/select.h>
#endif
#if __has_include(<sys/sem.h>)
#include <sys/sem.h>
#endif
#if __has_include(<sys/sendfile.h>)
#include <sys/sendfile.h>
#endif
#if __has_include(<sys/shm.h>)
#include <sys/shm.h>
#endif
#if __has_include(<sys/socket.h>)
#include <sys/socket.h>
#endif
#if __has_include(<sys/stat.h>)
#include <sys/stat.h>
#endif
#if __has_include(<sys/statfs.h>)
#include <sys/statfs.h>
#endif
#if __has_include(<sys/statvfs.h>)
#include <sys/statvfs.h>
#endif
#if __has_include(<sys/sysinfo.h>)
#include <sys/sysinfo.h>
#endif
#if __has_include(<sys/time.h>)
#include <sys/time.h>
#endif
#if __has_include(<sys/timerfd.h>)
#include <sys/timerfd.h>
#endif
#if __has_include(<sys/times.h>)
#include <sys/times.h>
#endif
#if __has_include(<sys/uio.h>)
#include <sys/uio.h>
#endif
#if __has_include(<sys/un.h>)
#include <sys/un.h>
#endif
#if __has_include(<sys/utsname.h>)
#include <sys/utsname.h>
#endif
#if __has_include(<sys/wait.h>)
#include <sys/wait.h>
#endif
#if __has_include(<sys/xattr.h>)
#include <sys/xattr.h>
#endif
#if __has_include(<termios.h>)
#include <termios.h>
#endif
#if __has_include(<time.h>)
#include <time.h>
#endif
#if __has_include(<ucontext.h>)
#include <ucontext.h>
#endif
#if __has_include(<unistd.h>)
#include <unistd.h>
#endif
#if __has_include(<utime.h>)
#include <utime.h>
#endif
#if __has_include(<wordexp.h>)
#include <wordexp.h>
#endif

// -fno-eliminate-unused-debug-types only keeps types the TU can see; an
// empty TU is still a valid TU, so nothing else is needed here.
int rust_ape_struct_probe_anchor;
