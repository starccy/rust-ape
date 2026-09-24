// pthread_attr_setstacksize: musl's PTHREAD_STACK_MIN is 2048 and cosmo's is
// 32768, so a size Rust considers valid gets EINVAL. Round it up instead.

#include <pthread.h>
#include <limits.h>

int __ape_shim_pthread_attr_setstacksize(pthread_attr_t *attr, size_t size) {
    if (size < PTHREAD_STACK_MIN) size = PTHREAD_STACK_MIN;
    return pthread_attr_setstacksize(attr, size);
}
