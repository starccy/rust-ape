# Patches

Every file here is a plain `diff -u` against pristine upstream source: `a/` is
what the crate ships, `b/` is the cosmo-adapted copy. Applying a patch to a
fresh download reproduces `vendor/` byte for byte and nothing else is touched.
`cargo xtask setup` does exactly that, so you normally never run `patch` by hand.

| Crate | Version |
| --- | --- |
| async-io | 2.6.0 |
| async-process | 2.5.0 |
| getrandom | 0.2.17 |
| libc | 0.2.189 |
| polling | 3.11.0 |

`library.patch` is a special case: it patches the Rust standard library from
the `rust-src` component of `nightly-YYYY-MM-dd` (You can find the exactly
version in `rust-toolchain.toml`).

Comments introduced by a patch start with `cosmo:` and explain why the hunk
is there; there is no separate design doc.

## Applying by hand

The directory name must be the crate name without the version:

```sh
curl -fsSLO https://static.crates.io/crates/libc/libc-0.2.189.crate
tar xf libc-0.2.189.crate && mv libc-0.2.189 libc
patch -p1 < libc.patch
```

## Verifying

To confirm `vendor/` has no undeclared changes, redo the above in a scratch
directory and compare:

```sh
diff -r libc ../vendor/patches/libc   # expect no output
```
