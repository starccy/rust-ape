/// custom define a macro to concat literal string and const string
macro_rules! concat_strs {
    ($($s:expr),* $(,)?) => {{
        const STRS: &[&str] = &[$($s),*];
        const LEN: usize = {
            let (mut len, mut i) = (0, 0);
            while i < STRS.len() { len += STRS[i].len(); i += 1; }
            len
        };
        const BYTES: [u8; LEN] = {
            let mut out = [0u8; LEN];
            let (mut out_i, mut i) = (0, 0);
            while i < STRS.len() {
                let s = STRS[i].as_bytes();
                let mut j = 0;
                while j < s.len() { out[out_i] = s[j]; out_i += 1; j += 1; }
                i += 1;
            }
            out
        };
        const OUT: &str = match str::from_utf8(&BYTES) {
            Ok(s) => s,
            Err(_) => unreachable!(),
        };
        OUT
    }};
}

pub(crate) use concat_strs;