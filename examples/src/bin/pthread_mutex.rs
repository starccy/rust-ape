//! pthread_mutex_t declared by the libc crate and driven by cosmo.
//!
//! The size of `pthread_mutex_t` differed between musl and cosmo; 
//! it has now been standardized to match the musl size. 
//! Test the correctness in this example.

use std::cell::UnsafeCell;
use std::mem::{align_of, size_of};

const CANARY: [u8; 64] = [0xa5; 64];
const THREADS: usize = 4;
const ROUNDS: u64 = 20_000;

#[repr(C)]
struct Guarded {
    mutex: UnsafeCell<libc::pthread_mutex_t>,
    canary: UnsafeCell<[u8; 64]>,
    counter: UnsafeCell<u64>,
}

unsafe impl Sync for Guarded {}

impl Guarded {
    fn check(&self, what: &str) {
        let canary = unsafe { *self.canary.get() };
        if canary != CANARY {
            let first = canary.iter().position(|&b| b != 0xa5).unwrap();
            panic!(
                "{what} wrote past the libc crate's {}-byte pthread_mutex_t \
                 (canary byte {first} changed)",
                size_of::<libc::pthread_mutex_t>()
            );
        }
    }
}

// the static initializer, the way C's PTHREAD_MUTEX_INITIALIZER is used
static STATIC: Guarded = Guarded {
    mutex: UnsafeCell::new(libc::PTHREAD_MUTEX_INITIALIZER),
    canary: UnsafeCell::new(CANARY),
    counter: UnsafeCell::new(0),
};

fn hammer(g: &'static Guarded, what: &str) {
    std::thread::scope(|s| {
        for _ in 0..THREADS {
            s.spawn(|| {
                for _ in 0..ROUNDS {
                    unsafe {
                        assert_eq!(libc::pthread_mutex_lock(g.mutex.get()), 0);
                        *g.counter.get() += 1;
                        assert_eq!(libc::pthread_mutex_unlock(g.mutex.get()), 0);
                    }
                }
            });
        }
    });
    g.check(what);
    let total = unsafe { *g.counter.get() };
    assert_eq!(total, THREADS as u64 * ROUNDS, "{what}: lost increments");
    println!("{what}: {THREADS} threads x {ROUNDS} locks, counter={total}, canary intact");
}

fn initialized(kind: libc::c_int) -> &'static Guarded {
    let g: &'static Guarded = Box::leak(Box::new(Guarded {
        mutex: UnsafeCell::new(unsafe { std::mem::zeroed() }),
        canary: UnsafeCell::new(CANARY),
        counter: UnsafeCell::new(0),
    }));
    unsafe {
        let mut attr: libc::pthread_mutexattr_t = std::mem::zeroed();
        assert_eq!(libc::pthread_mutexattr_init(&mut attr), 0);
        assert_eq!(libc::pthread_mutexattr_settype(&mut attr, kind), 0);
        assert_eq!(libc::pthread_mutex_init(g.mutex.get(), &attr), 0);
        libc::pthread_mutexattr_destroy(&mut attr);
    }
    g.check("pthread_mutex_init");
    g
}

fn main() {
    println!(
        "libc::pthread_mutex_t: {} bytes, align {}",
        size_of::<libc::pthread_mutex_t>(),
        align_of::<libc::pthread_mutex_t>()
    );

    hammer(&STATIC, "PTHREAD_MUTEX_INITIALIZER");

    hammer(initialized(libc::PTHREAD_MUTEX_NORMAL), "normal mutex");

    let rec = initialized(libc::PTHREAD_MUTEX_RECURSIVE);
    unsafe {
        for _ in 0..3 {
            assert_eq!(libc::pthread_mutex_lock(rec.mutex.get()), 0);
        }
        assert_eq!(libc::pthread_mutex_trylock(rec.mutex.get()), 0);
        for _ in 0..4 {
            assert_eq!(libc::pthread_mutex_unlock(rec.mutex.get()), 0);
        }
    }
    rec.check("recursive lock/unlock");
    hammer(rec, "recursive mutex");

    let ec = initialized(libc::PTHREAD_MUTEX_ERRORCHECK);
    unsafe {
        assert_eq!(libc::pthread_mutex_lock(ec.mutex.get()), 0);
        assert_eq!(libc::pthread_mutex_lock(ec.mutex.get()), libc::EDEADLK);
        assert_eq!(libc::pthread_mutex_unlock(ec.mutex.get()), 0);
        assert_eq!(libc::pthread_mutex_unlock(ec.mutex.get()), libc::EPERM);
    }
    ec.check("errorcheck lock/unlock");
    hammer(ec, "errorcheck mutex");

    for g in [&STATIC, rec, ec] {
        assert_eq!(unsafe { libc::pthread_mutex_destroy(g.mutex.get()) }, 0);
        g.check("pthread_mutex_destroy");
    }

    println!("\npthread mutex ok");
}
