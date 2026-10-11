#include "lp.h"
#ifdef _WIN32
#define API __declspec(dllexport)
int _fltused;
#else
#define API __attribute__((visibility("default")))
#endif
API long lp_neg(long v) { return -v; }
API void lp_out(long *out) { *out = -123456; }
API unsigned long lp_sum(const unsigned long *arr, int n) { unsigned long s = 0; for (int i = 0; i < n; ++i) s += arr[i]; return s; }
API void lp_scale(long *arr, int n, long by) { for (int i = 0; i < n; ++i) arr[i] *= by; }
/* writes a wide string that needs a surrogate pair on Windows */
API size_t lp_wname(wchar_t *buf, size_t n) {
    static const wchar_t s[] = L"中\U0001F600!";
    size_t i = 0;
    for (; i + 1 < n && s[i]; ++i) buf[i] = s[i];
    buf[i] = 0;
    return i;
}
API long lp_apply(lp_fn f, long v) { return f(v) + 1; }
API size_t lp_wlen(const wchar_t *s) { size_t n = 0; while (s[n]) ++n; return n; }
API long double lp_half(long double x) { return x / 2; }
/* fills every field so the caller can check each lands where it reads it */
API int lp_fill(struct outer *o) {
    o->tag = 7; o->in.a = -5; o->in.b = 9;
    o->arr[0] = 100; o->arr[1] = -200; o->arr[2] = 300;
    o->wc = 0x4e2d; o->flags = 5; o->mode = 17;
    o->name = "outer"; o->link = &o->in;
    return (int)sizeof(struct outer);
}
/* reads what the caller set, as a checksum */
API long lp_check(const struct outer *o) {
    return o->tag + o->in.a * 10 + o->in.b * 100 + o->arr[0] + o->arr[1] + o->arr[2]
         + o->wc + o->flags * 1000 + o->mode * 10000;
}
API int lp_plain(struct plain *p) { p->x = 1; p->y = 2.5; p->p = p; return (int)sizeof(*p); }
/* small integers, which Apple's arm64 has the caller widen; each lands in
   its own decimal range so a stray upper half shows */
API int lp_widen(unsigned char c, signed char s, short h, _Bool b, char k) {
    return c + s * 1000 + h * 100000 + (b ? 7 : 0) + k * 1000000;
}
/* a callback returning a small integer, read as the whole register */
API int lp_capply(lp_cfn f, short v) { return f(v); }
API struct inner lp_inner_val(struct inner in) { struct inner r = { in.a * 2, in.b * 2 }; return r; }
API long lp_inner_sum(struct inner in, long extra) { return in.a + in.b + extra; }
API struct plain lp_plain_val(struct plain p) { p.x += 1; p.y *= 2; return p; }
API long double lp_ld_fill(struct ld *p) { p->i = 3; p->x = 1.25L; return p->x * 2; }
/* integers and floating point interleaved, past the four register
   positions Windows has, for the thunks that call a function pointer */
API double lp_mix(int a, float b, long long c, double d, int e, float f, int g, double h) {
    return a + b * 10 + c * 100 + d * 1000 + e * 10000 + f * 100000 + g * 1000000 + h * 10000000;
}
API float lp_mixf(float a, int b, float c) { return a * 100 + b * 10 + c; }
/* more arguments than registers, the last three narrow: Apple's arm64
   packs those on the stack at their own sizes */
API long lp_ten(int a, int b, int c, int d, int e, int f, int g, int h, short i, char j, _Bool k) {
    return a + 2L * b + 3L * c + 4L * d + 5L * e + 6L * f + 7L * g + 8L * h + 100L * i + 10000L * j + 1000000L * k;
}
/* hands a callback the va_list of its own variadic arguments, as
   av_log_set_callback's callback gets one */
API void lp_vcall(lp_vfn f, void *ctx, const char *fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    f(ctx, fmt, ap);
    va_end(ap);
}
/* a callback with more arguments than registers, the last ones narrow,
   which Apple's arm64 packs on the stack when calling it */
API long lp_apply9(lp_fn9 f) { return f(1, 2, 3, 4, 5, 6, 7, 8, (short)-9, (char)10, 1, 2.5f); }
/* a callback taking and returning a long double by value */
API long double lp_apply_ld(lp_ldfn f, long double x) { return f(3, x) + 1; }
/* unions by value, which libffi is told about as structures the
   convention places the same way */
API long lp_union_l(union u v) { return v.l; }
API double lp_union_d(union u v, double k) { return v.d * k; }
API float lp_union_f(union uf v) { return v.b[0] + v.b[1] * 10; }
API int lp_union_m(union um v) { return v.i; }
API long lp_hu(struct hu h) { return h.tag * 1000 + h.v.l; }
API union u lp_union_ret(long x) { union u r; r.l = x * 2; return r; }
/* a callback taking a long, an unsigned long and a wchar_t, called with
   the registers dirty above the width Windows gives those; and a wchar_t
   returned the same way */
#if defined(_WIN32) && defined(__x86_64__)
__asm__(
    ".text\n"
    ".globl lp_apply_long\n"
    "lp_apply_long:\n"
    "  push %rbp\n"
    "  mov %rsp,%rbp\n"
    "  and $-16,%rsp\n"
    "  sub $32,%rsp\n"
    "  mov %rcx,%rax\n"
    "  movabs $0xdeadbeeffffffffb,%rcx\n"
    "  movabs $0xcafebabe00000007,%rdx\n"
    "  movabs $0xfeedface0000007a,%r8\n"
    "  call *%rax\n"
    "  mov %rbp,%rsp\n"
    "  pop %rbp\n"
    "  ret\n"
    ".globl lp_wchar_ret\n"
    "lp_wchar_ret:\n"
    "  movabs $0xdeadbeefcafe0000,%rax\n"
    "  movzwl %cx,%ecx\n"
    "  or %rcx,%rax\n"
    "  ret\n");
#else
API long lp_apply_long(lp_lfn f) { return f(-5, 7, L'z'); }
API wchar_t lp_wchar_ret(int c) { return (wchar_t)c; }
#endif
API long lp_call_hooks(const struct lp_hooks *h) { return lp_apply_long(h->f); }
