//! One host's view of a header, read off bindgen's output for that
//! target: types, records, their sizes and offsets, the functions.

use std::collections::HashMap;

use quote::ToTokens;

use super::Host;

#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) enum Ty {
    Prim(&'static str),
    Ptr,
    Array(Box<Ty>, usize),
    Rec(String),
    Other(String),
}

#[derive(Clone, Debug)]
pub(super) struct Bitfield {
    pub(super) unit: String,
    pub(super) name: String,
    pub(super) ret: syn::Type,
    pub(super) offset: usize,
    pub(super) width: u8,
}

pub(super) struct Record {
    pub(super) fields: Vec<(String, syn::Type)>,
    union: bool,
}

pub(super) struct Model {
    pub(super) host: Host,
    pub(super) aliases: HashMap<String, syn::Type>,
    pub(super) records: HashMap<String, Record>,
    pub(super) sizes: HashMap<String, (usize, usize)>,
    pub(super) offsets: HashMap<(String, String), usize>,
    pub(super) bitfields: HashMap<String, Vec<Bitfield>>,
    pub(super) fns: HashMap<String, syn::ForeignItemFn>,
}

pub(super) fn last_ident(ty: &syn::Type) -> Option<String> {
    match ty {
        syn::Type::Path(p) => p.path.segments.last().map(|s| s.ident.to_string()),
        _ => None,
    }
}

fn int_of(e: &syn::Expr) -> Option<usize> {
    match e {
        syn::Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Int(i),
            ..
        }) => i.base10_parse().ok(),
        _ => None,
    }
}

impl Model {
    pub(super) fn new(host: Host, file: &syn::File) -> Model {
        let mut m = Model {
            host,
            aliases: HashMap::new(),
            records: HashMap::new(),
            sizes: HashMap::new(),
            offsets: HashMap::new(),
            bitfields: HashMap::new(),
            fns: HashMap::new(),
        };
        for item in &file.items {
            match item {
                syn::Item::Type(t) => {
                    m.aliases.insert(t.ident.to_string(), (*t.ty).clone());
                }
                syn::Item::Struct(s) => {
                    let fields = s
                        .fields
                        .iter()
                        .filter_map(|f| Some((f.ident.as_ref()?.to_string(), f.ty.clone())))
                        .collect();
                    m.records.insert(
                        s.ident.to_string(),
                        Record {
                            fields,
                            union: false,
                        },
                    );
                }
                syn::Item::Union(u) => {
                    let fields = u
                        .fields
                        .named
                        .iter()
                        .filter_map(|f| Some((f.ident.as_ref()?.to_string(), f.ty.clone())))
                        .collect();
                    m.records.insert(
                        u.ident.to_string(),
                        Record {
                            fields,
                            union: true,
                        },
                    );
                }
                syn::Item::Const(c) if c.ident == "_" => m.layout_checks(&c.expr),
                syn::Item::Impl(i) if i.trait_.is_none() => {
                    if let Some(name) = last_ident(&i.self_ty) {
                        m.bitfield_getters(&name, i);
                    }
                }
                syn::Item::ForeignMod(b) => {
                    for fi in &b.items {
                        if let syn::ForeignItem::Fn(f) = fi {
                            m.fns.insert(f.sig.ident.to_string(), f.clone());
                        }
                    }
                }
                _ => {}
            }
        }
        m
    }

    // bindgen's `const _: () = { ["Size of s"][size_of::<s>() - 112usize]; .. }`
    fn layout_checks(&mut self, e: &syn::Expr) {
        let syn::Expr::Block(b) = e else { return };
        for st in &b.block.stmts {
            let syn::Stmt::Expr(syn::Expr::Index(ix), _) = st else {
                continue;
            };
            let syn::Expr::Array(a) = &*ix.expr else {
                continue;
            };
            let Some(syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(s),
                ..
            })) = a.elems.first()
            else {
                continue;
            };
            let syn::Expr::Binary(bin) = &*ix.index else {
                continue;
            };
            let Some(n) = int_of(&bin.right) else {
                continue;
            };
            let s = s.value();
            if let Some(name) = s.strip_prefix("Size of ") {
                self.sizes.entry(name.to_string()).or_insert((0, 1)).0 = n;
            } else if let Some(name) = s.strip_prefix("Alignment of ") {
                self.sizes.entry(name.to_string()).or_insert((0, 1)).1 = n;
            } else if let Some(rest) = s.strip_prefix("Offset of field: ") {
                if let Some((rec, field)) = rest.split_once("::") {
                    self.offsets.insert((rec.to_string(), field.to_string()), n);
                }
            }
        }
    }

    // `pub fn flags(&self) -> c_uint { .. self._bitfield_1.get_const::<0usize, 3u8>() .. }`
    fn bitfield_getters(&mut self, rec: &str, i: &syn::ItemImpl) {
        for it in &i.items {
            let syn::ImplItem::Fn(f) = it else { continue };
            if f.sig.inputs.len() != 1 {
                continue;
            }
            let syn::ReturnType::Type(_, ret) = &f.sig.output else {
                continue;
            };
            let body = f.block.to_token_stream().to_string().replace(' ', "");
            let Some(at) = body.find("self._bitfield_") else {
                continue;
            };
            let unit: String = body[at + 5..]
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            let (off, width) = if let Some(p) = body.find("get_const::<") {
                let r = &body[p + 12..];
                let off: String = r.chars().take_while(|c| c.is_ascii_digit()).collect();
                let r = &r[r.find(',').map(|x| x + 1).unwrap_or(0)..];
                let w: String = r.chars().take_while(|c| c.is_ascii_digit()).collect();
                (off, w)
            } else if let Some(p) = body.find(".get(") {
                let r = &body[p + 5..];
                let off: String = r.chars().take_while(|c| c.is_ascii_digit()).collect();
                let r = &r[r.find(',').map(|x| x + 1).unwrap_or(0)..];
                let w: String = r.chars().take_while(|c| c.is_ascii_digit()).collect();
                (off, w)
            } else {
                continue;
            };
            let (Ok(offset), Ok(width)) = (off.parse(), width.parse()) else {
                continue;
            };
            self.bitfields
                .entry(rec.to_string())
                .or_default()
                .push(Bitfield {
                    unit,
                    name: f.sig.ident.to_string(),
                    ret: (**ret).clone(),
                    offset,
                    width,
                });
        }
    }

    pub(super) fn resolve(&self, ty: &syn::Type) -> Ty {
        self.resolve_depth(ty, 0)
    }

    fn resolve_depth(&self, ty: &syn::Type, depth: usize) -> Ty {
        if depth > 32 {
            return Ty::Other("recursive alias".into());
        }
        match ty {
            syn::Type::Ptr(_) | syn::Type::FnPtr(_) => Ty::Ptr,
            syn::Type::Array(a) => {
                let n = int_of(&a.len).unwrap_or(0);
                Ty::Array(Box::new(self.resolve_depth(&a.elem, depth + 1)), n)
            }
            syn::Type::Paren(p) => self.resolve_depth(&p.elem, depth + 1),
            syn::Type::Path(p) => {
                let Some(seg) = p.path.segments.last() else {
                    return Ty::Other(String::new());
                };
                let id = seg.ident.to_string();
                let win = self.host == Host::Win;
                let prim = match id.as_str() {
                    "c_char" | "c_schar" | "i8" => "i8",
                    "c_uchar" | "u8" => "u8",
                    "c_short" | "i16" => "i16",
                    "c_ushort" | "u16" => "u16",
                    "c_int" | "i32" => "i32",
                    "c_uint" | "u32" => "u32",
                    "c_long" => {
                        if win {
                            "i32"
                        } else {
                            "i64"
                        }
                    }
                    "c_ulong" => {
                        if win {
                            "u32"
                        } else {
                            "u64"
                        }
                    }
                    "c_longlong" | "i64" => "i64",
                    "c_ulonglong" | "u64" => "u64",
                    "i128" => "i128",
                    "u128" => "u128",
                    "isize" => "isize",
                    "usize" => "usize",
                    "c_float" | "f32" => "f32",
                    "c_double" | "f64" => "f64",
                    "bool" => "bool",
                    "LongDouble" => "u128",
                    "Callback" | "Option" => return Ty::Ptr,
                    _ => "",
                };
                if !prim.is_empty() {
                    return Ty::Prim(prim);
                }
                if let Some(t) = self.aliases.get(&id) {
                    return self.resolve_depth(t, depth + 1);
                }
                if self.records.contains_key(&id) {
                    return Ty::Rec(id);
                }
                Ty::Other(id)
            }
            _ => Ty::Other(ty.to_token_stream().to_string()),
        }
    }

    pub(super) fn size_of(&self, t: &Ty) -> Option<usize> {
        match t {
            Ty::Prim(p) => Some(prim_size(p)),
            Ty::Ptr => Some(8),
            Ty::Array(e, n) => Some(self.size_of(e)? * n),
            Ty::Rec(r) => self.sizes.get(r).map(|s| s.0),
            Ty::Other(_) => None,
        }
    }

    // where each bitfield unit starts, which bindgen doesn't check: right
    // after the field before it, rounded up to the alignment its
    // `_bitfield_align_N` field asks for
    pub(super) fn unit_offsets(&self, rec: &str) -> HashMap<String, usize> {
        let mut out = HashMap::new();
        let Some(r) = self.records.get(rec) else {
            return out;
        };
        let mut end = 0usize;
        let mut align = 1usize;
        for (name, ty) in &r.fields {
            if name.starts_with("_bitfield_align_") {
                if let syn::Type::Array(a) = ty {
                    align = self.size_of(&self.resolve(&a.elem)).unwrap_or(1);
                }
                continue;
            }
            if name.starts_with("_bitfield_") {
                let off = end.div_ceil(align) * align;
                out.insert(name.clone(), off);
                let n = unit_len(ty).unwrap_or(0);
                end = off + n;
                align = 1;
                continue;
            }
            if name.starts_with("__bindgen_padding") {
                // explicit padding has no layout check of its own
                end += self.size_of(&self.resolve(ty)).unwrap_or(0);
            } else if let Some(&off) = self.offsets.get(&(rec.to_string(), name.clone())) {
                end = off + self.size_of(&self.resolve(ty)).unwrap_or(0);
            }
        }
        out
    }

    // the members of `rec` as libffi wants them listed, or None for
    // something it can't describe. `arm` classifies for AAPCS64 (Linux
    // arm64 and Apple's arm64), otherwise for x86-64
    pub(super) fn ffi_members(&self, rec: &str, depth: usize, arm: bool) -> Option<Vec<String>> {
        let r = self.records.get(rec)?;
        if depth > 8 {
            return None;
        }
        if r.union {
            return self.union_members(rec, depth, arm);
        }
        let mut out = Vec::new();
        let mut align = 1usize;
        for (name, ty) in &r.fields {
            if name.starts_with("_bitfield_align_") {
                if let syn::Type::Array(a) = ty {
                    align = self.size_of(&self.resolve(&a.elem)).unwrap_or(1);
                }
                continue;
            }
            if name.starts_with("_bitfield_") {
                let n = unit_len(ty)?;
                let (unit, count) = if n % align == 0 {
                    (align, n / align)
                } else {
                    (1, n)
                };
                let k = match unit {
                    8 => "U64",
                    4 => "U32",
                    2 => "U16",
                    _ => "U8",
                };
                out.extend(std::iter::repeat_n(k.to_string(), count));
                align = 1;
                continue;
            }
            self.ffi_kinds(&self.resolve(ty), depth, arm, &mut out)?;
        }
        Some(out)
    }

    // libffi has no union type, so a union goes as a structure the
    // convention places the same way: on x86-64 each eightbyte is a
    // double if only floating point members cover it and an integer
    // otherwise, on arm64 a union of one floating point type is a
    // homogeneous aggregate of it and anything else is integers, and
    // Windows never puts a union in a vector register. The units are
    // the union's own alignment, so a nested one keeps its place
    fn union_members(&self, rec: &str, depth: usize, arm: bool) -> Option<Vec<String>> {
        let r = self.records.get(rec)?;
        let &(size, align) = self.sizes.get(rec)?;
        if size == 0 || align == 0 || size % align != 0 {
            return None;
        }
        let mut leaves = Vec::new();
        for (name, ty) in &r.fields {
            if name.starts_with("_bitfield_align_") {
                continue;
            }
            if name.starts_with("_bitfield_") {
                let n = unit_len(ty)?;
                for i in 0..n {
                    leaves.push((i, "u8"));
                }
                continue;
            }
            self.leaves(&self.resolve(ty), 0, depth + 1, &mut leaves)?;
        }
        let unit = align.min(8);
        let float = |p: &str| p == "f32" || p == "f64";
        let kind = |u: usize, f: Option<&str>| -> &'static str {
            match (u, f) {
                (8, Some("f64")) | (8, Some("f32")) => "F64",
                (4, Some(_)) => "F32",
                (8, _) => "U64",
                (4, _) => "U32",
                (2, _) => "U16",
                _ => "U8",
            }
        };
        let mut out = Vec::new();
        if arm && self.host != Host::Win {
            let first = leaves.first().map(|l| l.1);
            if let Some(f) = first
                && float(f)
                && leaves.iter().all(|l| l.1 == f)
                && size / prim_size(f) <= 4
            {
                out.extend(std::iter::repeat_n(
                    if f == "f64" { "F64" } else { "F32" }.to_string(),
                    size / prim_size(f),
                ));
                return Some(out);
            }
            out.extend(std::iter::repeat_n(
                kind(unit, None).to_string(),
                size / unit,
            ));
            return Some(out);
        }
        let mut at = 0;
        while at < size {
            let end = (at + 8).min(size);
            let only_floats = self.host != Host::Win
                && leaves.iter().all(|&(o, p)| {
                    let w = prim_size(p);
                    o >= end || o + w <= at || float(p)
                });
            let f = only_floats.then_some(if unit == 8 { "f64" } else { "f32" });
            let mut u = at;
            while u < end {
                out.push(kind(unit, f).to_string());
                u += unit;
            }
            at = end;
        }
        Some(out)
    }

    // the scalars a value of `t` at offset `at` is made of
    fn leaves(
        &self,
        t: &Ty,
        at: usize,
        depth: usize,
        out: &mut Vec<(usize, &'static str)>,
    ) -> Option<()> {
        if depth > 8 {
            return None;
        }
        match t {
            Ty::Prim("i128") | Ty::Prim("u128") => return None,
            Ty::Prim("bool") => out.push((at, "u8")),
            Ty::Prim(p) => out.push((at, p)),
            Ty::Ptr => out.push((at, "u64")),
            Ty::Array(e, n) => {
                let w = self.size_of(e)?;
                for i in 0..*n {
                    self.leaves(e, at + i * w, depth + 1, out)?;
                }
            }
            Ty::Rec(r) => {
                let rec = self.records.get(r)?;
                let units = self.unit_offsets(r);
                for (name, ty) in &rec.fields {
                    if name.starts_with("_bitfield_align_") {
                        continue;
                    }
                    if name.starts_with("_bitfield_") {
                        let off = *units.get(name)?;
                        for i in 0..unit_len(ty)? {
                            out.push((at + off + i, "u8"));
                        }
                        continue;
                    }
                    let off = if rec.union {
                        0
                    } else if name.starts_with("__bindgen_padding") {
                        continue;
                    } else {
                        *self.offsets.get(&(r.clone(), name.clone()))?
                    };
                    self.leaves(&self.resolve(ty), at + off, depth + 1, out)?;
                }
            }
            Ty::Other(_) => return None,
        }
        Some(())
    }

    fn ffi_kinds(&self, t: &Ty, depth: usize, arm: bool, out: &mut Vec<String>) -> Option<()> {
        match t {
            Ty::Prim(p) => match *p {
                "i128" | "u128" => {
                    // libffi has no 128 bit integer; a long double in a
                    // structure isn't told from one here
                    out.push("U64".into());
                    out.push("U64".into());
                }
                "bool" => out.push("U8".into()),
                "isize" => out.push("I64".into()),
                "usize" => out.push("U64".into()),
                p => out.push(p.to_uppercase()),
            },
            Ty::Ptr => out.push("Ptr".into()),
            Ty::Array(e, n) => {
                for _ in 0..*n {
                    self.ffi_kinds(e, depth, arm, out)?;
                }
            }
            Ty::Rec(r) => {
                let m = self.ffi_members(r, depth + 1, arm)?;
                out.push(format!("Struct(&[{}])", m.join(", ")));
            }
            Ty::Other(_) => return None,
        }
        Some(())
    }
}

fn prim_size(p: &str) -> usize {
    match p {
        "i8" | "u8" | "bool" => 1,
        "i16" | "u16" => 2,
        "i32" | "u32" | "f32" => 4,
        "i64" | "u64" | "f64" | "isize" | "usize" => 8,
        _ => 16,
    }
}

// the byte count of a `__BindgenBitfieldUnit<[u8; N]>`
fn unit_len(ty: &syn::Type) -> Option<usize> {
    let syn::Type::Path(p) = ty else { return None };
    let seg = p.path.segments.last()?;
    let syn::PathArguments::AngleBracketed(a) = &seg.arguments else {
        return None;
    };
    let Some(syn::GenericArgument::Type(syn::Type::Array(arr))) = a.args.first() else {
        return None;
    };
    int_of(&arr.len)
}

pub(super) fn is_internal_field(name: &str) -> bool {
    name.starts_with("_bitfield_") || name.starts_with("__bindgen_padding")
}
