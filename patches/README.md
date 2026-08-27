# Patches

Every file here is a plain `diff -u` against pristine upstream source, where
`a/` is what the crate ships and `b/` is the cosmo-adapted copy. Applying a patch to a
fresh download reproduces `vendor/` byte for byte and nothing else is touched.
`cargo xtask setup` does exactly that, so you normally never run `patch` by hand.

Current patched list:

| Crate | Version |
| --- | --- |
| errno | 0.3.14 |
| libc | 0.2.189 |

`library.patch` is a special case. It patches the Rust standard library from
the `rust-src` component of `nightly-YYYY-MM-dd` (the exact version is in
`rust-toolchain.toml`).

Comments introduced by a patch start with `cosmo:` and explain why the hunk
is there; there is no separate design doc.

## Changing one

Edit the vendored copy (`vendor/library` or `vendor/patches/<crate>`) and
regenerate:

```sh
./patches/regen.sh              # every patch
./patches/regen.sh errno        # just one
```

It diffs `vendor/` against a pristine upstream copy (rust-src from your rustup
for `library`, the `*.crate` in `cache/` for the rest). Before writing
anything it applies the new patch to clean upstream and checks that the result
reproduces `vendor/` byte for byte. Otherwise the existing patch is left alone
and the script exits non-zero.

Then rebuild, since the SDK only re-applies patches when a stamp changes:

```sh
cargo xtask setup
```

## Checking the symbols

```sh
./patches/check-symbols.sh
```

Cross-checks `shim/` against the patches. Every `__ape_shim_*` symbol the
shim defines must have a matching `link_name` redirect in a patch, so that
no shim silently goes unused.

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

## About these patches

Honestly, I'd rather not patch anything. Patches are tied to exact versions,
so every Rust or crate upgrade means redoing them, and that is painful to
maintain. What's here is the smallest set I could come up with.

They exist because Rust's standard library pins flag constants at compile
time. Since everything compiles against a Linux target, the binary
carries Linux values wherever it runs, and on other platforms those values are
simply wrong. The result ranges from errors and hangs to subtler cases where
the program silently behaves differently. Cosmopolitan avoids this by making
these values runtime globals, resolved on startup for whatever host it finds.

The obvious fix would be to replace every constant the standard library
(mostly the libc crate) uses with cosmo's variables. That does solve the
problem, but the constants are scattered across a huge number of places, the
patch would get very large, some constants would probably still be missed, and
redoing all of it for the next Rust version would be a disaster.

So instead there is a shim layer in between. The libc functions that consume
these constants are redirected to wrapper functions in `shim/`, which
translate the Linux values into cosmo's internally. This way only a small set
of functions needs patching. Still not few, but at least nowhere near as
scattered as the constants. See [shim/README.md](../shim/README.md) for how it
works.

Besides constants, there are also struct layouts that musl and cosmo disagree
on. Those are split between the two sides. The libc patch re-declares
aarch64's `struct stat` to match cosmo's layout, while structs that need
repacking at runtime, like `sigaction`, are handled inside the shim.
