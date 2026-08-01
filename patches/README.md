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

## Changing one

Edit the vendored copy: `vendor/library` or `vendor/patches/<crate>` and
regenerate:

```sh
./patches/regen.sh              # every patch
./patches/regen.sh async-io     # just one
```

It fetches pristine upstream (rust-src from your rustup for `library`, the
`.crate` in `cache/` for the rest, at the version `vendor/.stamps` records),
diffs it against `vendor/`, and strips timestamps so unrelated runs don't churn
the files.

Nothing is written until the result round-trips: applying the fresh patch to a
clean upstream copy has to reproduce `vendor/` byte for byte. If it doesn't, the
existing patch is left alone and the script exits non-zero. This check catches
patches that look fine but silently drop a file.

Then rebuild, since the SDK only re-applies patches when a stamp changes:

```sh
cargo xtask setup
```

## Applying by hand

`cargo xtask setup` does this for you; you only need it to inspect a patch
without a working SDK. The directory name must be the crate name without the
version:

```sh
curl -fsSLO https://static.crates.io/crates/libc/libc-0.2.189.crate
tar xf libc-0.2.189.crate && mv libc-0.2.189 libc
patch -p1 < libc.patch
diff -r libc ../vendor/patches/libc   # expect no output
```
