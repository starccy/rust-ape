use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Guard(&'static str, Arc<AtomicUsize>);

impl Drop for Guard {
    fn drop(&mut self) {
        println!("  unwind past {} (drop ran)", self.0);
        self.1.fetch_add(1, Ordering::SeqCst);
    }
}

fn main() {
    // The panic hook stays quiet so the log doesn't read like a real failure.
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));

    let dropped = Arc::new(AtomicUsize::new(0));
    caught(&dropped);
    threads();
    smol_tasks();

    std::panic::set_hook(prev);
    println!("\npanic unwind ok");
}

fn deep(n: usize, dropped: &Arc<AtomicUsize>) {
    let _g = Guard(
        match n {
            0 => "frame0",
            1 => "frame1",
            2 => "frame2",
            _ => "frameN",
        },
        dropped.clone(),
    );
    if n == 0 {
        panic!("thrown from the bottom");
    }
    deep(n - 1, dropped);
}

fn caught(dropped: &Arc<AtomicUsize>) {
    let r = catch_unwind(AssertUnwindSafe(|| deep(2, dropped)));
    let payload = r.expect_err("catch_unwind saw no panic");
    let msg = payload.downcast_ref::<&str>().copied().unwrap_or("<not a &str>");
    assert_eq!(msg, "thrown from the bottom", "payload came back as {msg:?}");
    assert_eq!(dropped.load(Ordering::SeqCst), 3, "not every frame's Drop ran");
    println!("catch_unwind: caught {msg:?}, 3 destructors ran");

    // The process is still usable afterwards.
    assert_eq!(catch_unwind(|| 1 + 1).expect("second catch_unwind"), 2);
}

fn threads() {
    let h = std::thread::spawn(|| panic!("in a thread"));
    let err = h.join().expect_err("thread panic vanished");
    assert_eq!(err.downcast_ref::<&str>().copied(), Some("in a thread"));

    // The rest of the program keeps going.
    let h = std::thread::spawn(|| 7);
    assert_eq!(h.join().expect("post-panic thread"), 7);
    println!("threads: panic stayed in its thread, join reported it");
}

fn smol_tasks() {
    smol::block_on(async {
        let t = smol::spawn(async { panic!("in a smol task") });
        // smol turns a panicked task into a cancelled one when it's awaited
        // through catch_unwind; awaiting directly resumes the panic here.
        let r = catch_unwind(AssertUnwindSafe(|| smol::block_on(t)));
        assert!(r.is_err(), "smol task panic didn't propagate");

        let t = smol::spawn(async { 21 * 2 });
        assert_eq!(t.await, 42);
    });
    println!("smol: task panic propagated, executor kept running");
}
