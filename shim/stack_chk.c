// Fills a gap in cosmo's stack-protector support.
//
// Third-party -sys crates can force -fstack-protector back on regardless of
// our CFLAGS (ring 0.16's build.rs hardcodes it), and cosmocc's GCC is
// configured for the global-guard flavor, where protected functions
// reference the __stack_chk_guard symbol. cosmo 4.0.2 ships that symbol in
// the x86_64 libcosmo.a but forgot it in the aarch64 one, so such crates
// link on one arch and fail to link on the other.
//
// A weak definition fills the gap. Where libcosmo has the real (strong)
// symbol this one yields; where it doesn't, this one links. A fixed canary
// is weaker protection than a randomized one, but the alternative is a
// build that doesn't link at all; __stack_chk_fail exists on both arches.

__attribute__((weak)) unsigned long __stack_chk_guard = 0x00c0ffee5afe7ea1ul;
