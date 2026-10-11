//! Local time: does the host's timezone survive the trip to a non-Linux box?
//!
//! The Rust world here is built for linux-musl, so every date crate compiles
//! its unix path and goes looking for /etc/localtime and /usr/share/zoneinfo.
//! Windows has neither, and the usual failure mode is silence: local time
//! quietly becomes UTC. Same shape as the certificate store, with one piece of
//! luck attached -- cosmo puts the host's zone *name* in `TZ` on Windows, so
//! what is missing is only the database behind the name.
//!
//! That database rides along in the zip store (see examples/bundle.toml):
//! chrono asks for /usr/share/zoneinfo/<name>, cosmo serves it from /zip on
//! any host that lacks the directory. So chrono now has to agree
//! with libc everywhere, and this asserts it.

use std::ffi::CStr;
use std::path::Path;

// The musl libc crate has no tzset; cosmo does.
unsafe extern "C" {
    fn tzset();
}

/// What cosmo's own localtime_r says. Ground truth here: it asks the host
/// (GetTimeZoneInformation on Windows) instead of reading tzdata off disk.
fn libc_offset(t: i64) -> (i64, String) {
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        tzset();
        assert!(
            !libc::localtime_r(&t, &mut tm).is_null(),
            "localtime_r failed"
        );
        let zone = if tm.tm_zone.is_null() {
            String::from("?")
        } else {
            CStr::from_ptr(tm.tm_zone).to_string_lossy().into_owned()
        };
        (tm.tm_gmtoff as i64, zone)
    }
}

fn main() {
    // A fixed instant so every layer is asked about the same moment and no DST
    // boundary can slip between two readings.
    let stamp = 1_786_470_000i64;

    println!("TZ={:?}", std::env::var("TZ").ok());
    for p in ["/etc/localtime", "/usr/share/zoneinfo"] {
        println!(
            "{p}: {}",
            if Path::new(p).exists() {
                "present"
            } else {
                "MISSING"
            }
        );
    }

    let (libc_off, libc_zone) = libc_offset(stamp);
    println!("\nlibc localtime_r : offset {libc_off:+}s  zone {libc_zone}");

    // chrono reads TZ, then /etc/localtime, and parses the TZif itself.
    let chrono_off = {
        use chrono::{Local, TimeZone};
        let dt = Local
            .timestamp_opt(stamp, 0)
            .single()
            .expect("ambiguous local time");
        let off = dt.offset().local_minus_utc() as i64;
        println!("chrono Local     : offset {off:+}s  {dt}");
        off
    };

    // jiff carries its own tzdb reader. On unix that reads /usr/share/zoneinfo;
    // `tzdb-bundle-always` (see Cargo.toml) compiles the database in instead,
    // which is what makes the IANA name in TZ resolvable on Windows.
    let jiff_off = {
        use jiff::{Timestamp, tz::TimeZone};
        let ts = Timestamp::from_second(stamp).unwrap();
        let tz = TimeZone::system();
        let zoned = ts.to_zoned(tz.clone());
        let off = zoned.offset().seconds() as i64;
        println!(
            "jiff system tz   : offset {off:+}s  {zoned}  (iana {:?})",
            tz.iana_name()
        );
        off
    };

    // Named zones out of the bundled database: no host resource involved, so
    // these hold everywhere and are the one thing worth asserting hard.
    {
        use jiff::{Timestamp, tz::TimeZone};
        let ts = Timestamp::from_second(stamp).unwrap();
        for (name, want) in [("Asia/Shanghai", 8 * 3600), ("America/New_York", -4 * 3600)] {
            let tz =
                TimeZone::get(name).unwrap_or_else(|e| panic!("bundled tzdb lacks {name}: {e}"));
            let zoned = ts.to_zoned(tz);
            println!("jiff bundled tzdb: {name} -> {zoned}");
            // The stamp is in August, so New York is on daylight time.
            assert_eq!(zoned.offset().seconds() as i64, want, "{name} offset");
        }
    }

    println!("\noffsets: libc {libc_off:+}s, chrono {chrono_off:+}s, jiff {jiff_off:+}s");

    // chrono cannot compile a database in; the bundled /usr/share/zoneinfo
    // is what lets it resolve the IANA name in TZ off Linux. Before the
    // bundle this was a warning on Windows.
    assert_eq!(
        chrono_off, libc_off,
        "the host is at {libc_off:+}s ({libc_zone}) but chrono says {chrono_off:+}s. \
         It resolved neither /etc/localtime nor the IANA name in TZ through the \
         bundled zoneinfo. Is the bundle in this binary (unzip -l)?"
    );

    // jiff, with the database compiled in, has everything it needs on every
    // host: the name from cosmo's TZ and the rules from the bundle.
    assert_eq!(
        jiff_off, libc_off,
        "the host is at {libc_off:+}s ({libc_zone}) but jiff says {jiff_off:+}s, \
         even with a bundled tzdb. Did cosmo stop exporting TZ?"
    );

    println!("\nlocal time ok");
}
