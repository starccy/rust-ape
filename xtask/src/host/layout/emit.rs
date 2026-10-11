//! The Rust emitted for a record laid out differently, and its libffi
//! description.

use super::Host;
use super::analysis::{Analysis, tokens};
use super::model::{Ty, is_internal_field, last_ident};

// `match ape::host::layout::host() { 1 => .., 2 => .., _ => base }`, or
// just `base` where every host has it
fn branch(arms: &[(Host, String)], base: &str) -> String {
    let arms: Vec<String> = arms
        .iter()
        .filter(|(_, s)| s != base)
        .map(|(h, s)| format!("{} => {{ {s} }}", *h as usize))
        .collect();
    if arms.is_empty() {
        base.to_string()
    } else {
        format!(
            "(match ape::host::layout::host() {{ {}, _ => {{ {base} }} }})",
            arms.join(", ")
        )
    }
}

// the replacement for structure `rec`. The accessors are written once
// with $R/$W for the receiver and $P/$M for the base pointer, then given
// to both the structure and At<rec>
pub(super) fn emit_record(a: &mut Analysis, rec: &str) -> String {
    let models = a.all();
    let u = a.base;
    let (us, ua) = u.sizes[rec];
    let sizes: Vec<(Host, (usize, usize))> = models
        .iter()
        .map(|m| (m.host, m.sizes.get(rec).copied().unwrap_or((us, ua))))
        .collect();
    let align = sizes.iter().map(|s| s.1.1).max().unwrap_or(ua);
    let size = sizes
        .iter()
        .map(|s| s.1.0)
        .max()
        .unwrap_or(us)
        .div_ceil(align)
        * align;
    let mut m = Vec::<String>::new();
    let ufields = &u.records[rec].fields;
    for (name, ut) in ufields {
        if is_internal_field(name) {
            continue;
        }
        let key = (rec.to_string(), name.clone());
        let Some(&uo) = u.offsets.get(&key) else {
            a.warnings.push(format!(
                "{rec}.{name} has no offset check, no accessor for it"
            ));
            continue;
        };
        // the field on each host: offset and type
        let mut views: Vec<(Host, usize, &syn::Type)> = Vec::new();
        let mut missing = false;
        for md in &models {
            let ft = md
                .records
                .get(rec)
                .and_then(|r| r.fields.iter().find(|(n, _)| n == name))
                .map(|(_, t)| t);
            match (md.offsets.get(&key), ft) {
                (Some(&o), Some(t)) => views.push((md.host, o, t)),
                _ => missing = true,
            }
        }
        if missing {
            a.warnings.push(format!(
                "{rec}.{name} isn't on every host, no accessor for it"
            ));
            continue;
        }
        let tu = u.resolve(ut);
        let others: Vec<(Host, &syn::Type)> =
            views.iter().skip(1).map(|(h, _, t)| (*h, *t)).collect();
        let ld = a.is_long_double(ut, &others);
        let api = if ld {
            "ape::host::layout::LongDouble".to_string()
        } else {
            tokens(ut)
        };
        let off = branch(
            &views
                .iter()
                .map(|(h, o, _)| (*h, format!("{o}usize")))
                .collect::<Vec<_>>(),
            &format!("{uo}usize"),
        );
        match &tu {
            Ty::Prim(pu) => {
                let mut gets = Vec::new();
                let mut sets = Vec::new();
                for (h, o, t) in &views {
                    let md = a.model(*h).unwrap();
                    let th = md.resolve(t);
                    let Ty::Prim(ph) = th else {
                        a.warnings.push(format!(
                            "{rec}.{name} is a {} on {}, no accessor for it",
                            tokens(t),
                            h.name()
                        ));
                        continue;
                    };
                    let (g, s) = if ph == *pu {
                        (
                            format!("p.add({o}).cast::<{api}>().read_unaligned()"),
                            format!("p.add({o}).cast::<{api}>().write_unaligned(v)"),
                        )
                    } else if ld && ph == "f64" {
                        (
                            format!(
                                "ape::host::layout::LongDouble::from_f64(p.add({o}).cast::<f64>().read_unaligned())"
                            ),
                            format!("p.add({o}).cast::<f64>().write_unaligned(v.to_f64())"),
                        )
                    } else {
                        (
                            format!("p.add({o}).cast::<{ph}>().read_unaligned() as {api}"),
                            format!("p.add({o}).cast::<{ph}>().write_unaligned(v as {ph})"),
                        )
                    };
                    gets.push((*h, g));
                    sets.push((*h, s));
                }
                if *pu == "u128" && !ld && a.others.iter().any(|m| m.host == Host::Xnu) {
                    a.warnings.push(format!(
                        "{rec}.{name} is a u128 on every host, taken to be a 128 bit integer"
                    ));
                }
                let get = branch(&gets, &gets[0].1);
                let set = branch(&sets, &sets[0].1);
                m.push(format!(
                    "#[inline] pub fn {name}($R) -> {api} {{ let p = $P; unsafe {{ {get} }} }}\n\
                     #[inline] pub fn set_{name}($W, v: {api}) {{ let p = $M; unsafe {{ {set} }} }}"
                ));
            }
            Ty::Ptr => {
                m.push(format!(
                    "#[inline] pub fn {name}($R) -> {api} {{ let p = $P; unsafe {{ p.add({off}).cast::<{api}>().read_unaligned() }} }}\n\
                     #[inline] pub fn set_{name}($W, v: {api}) {{ let p = $M; unsafe {{ p.add({off}).cast::<{api}>().write_unaligned(v) }} }}"
                ));
            }
            Ty::Rec(r) => {
                m.push(format!(
                    "#[inline] pub fn {name}_ptr($R) -> *mut {api} {{ unsafe {{ $PM.add({off}) as *mut {api} }} }}"
                ));
                if a.differs(r) {
                    m.push(format!(
                        "#[inline] pub fn {name}($R) -> At<{api}> {{ At(self.{name}_ptr()) }}"
                    ));
                } else {
                    m.push(format!(
                        "#[inline] pub fn {name}($R) -> {api} {{ unsafe {{ self.{name}_ptr().read_unaligned() }} }}\n\
                         #[inline] pub fn set_{name}($W, v: {api}) {{ unsafe {{ self.{name}_ptr().write_unaligned(v) }} }}"
                    ));
                }
            }
            Ty::Array(ue, n) => {
                let syn::Type::Array(arr) = ut else { continue };
                let elem = tokens(&arr.elem);
                let mut strides = Vec::new();
                let mut elems = Vec::new();
                let mut bad = false;
                for (h, o, t) in &views {
                    let md = a.model(*h).unwrap();
                    let Ty::Array(e, _) = md.resolve(t) else {
                        bad = true;
                        break;
                    };
                    let Some(sz) = md.size_of(&e) else {
                        bad = true;
                        break;
                    };
                    strides.push((*h, format!("{o} + i * {sz}")));
                    elems.push((*h, *e));
                }
                if bad {
                    a.warnings.push(format!("{rec}.{name} is an array of something the generator can't size, no accessor for it"));
                    continue;
                }
                let at = branch(&strides, &strides[0].1);
                m.push(format!(
                    "#[inline] pub fn {name}_ptr($R, i: usize) -> *mut {elem} {{ assert!(i < {n}); unsafe {{ $PM.add({at}) as *mut {elem} }} }}"
                ));
                match &**ue {
                    Ty::Prim(pu) => {
                        let mut gets = Vec::new();
                        let mut sets = Vec::new();
                        for (h, e) in &elems {
                            let (g, s) = match e {
                                Ty::Prim(pe) if pe == pu => (
                                    "q.read_unaligned()".to_string(),
                                    "q.write_unaligned(v)".to_string(),
                                ),
                                Ty::Prim(pe) => (
                                    format!("(q as *const {pe}).read_unaligned() as {elem}"),
                                    format!("(q as *mut {pe}).write_unaligned(v as {pe})"),
                                ),
                                _ => continue,
                            };
                            gets.push((*h, g));
                            sets.push((*h, s));
                        }
                        let get = branch(&gets, &gets[0].1);
                        let set = branch(&sets, &sets[0].1);
                        m.push(format!(
                            "#[inline] pub fn {name}($R, i: usize) -> {elem} {{ let q = self.{name}_ptr(i); unsafe {{ {get} }} }}\n\
                             #[inline] pub fn set_{name}($W, i: usize, v: {elem}) {{ let q = self.{name}_ptr(i); unsafe {{ {set} }} }}"
                        ));
                    }
                    Ty::Ptr => {
                        m.push(format!(
                            "#[inline] pub fn {name}($R, i: usize) -> {elem} {{ unsafe {{ self.{name}_ptr(i).read_unaligned() }} }}\n\
                             #[inline] pub fn set_{name}($W, i: usize, v: {elem}) {{ unsafe {{ self.{name}_ptr(i).write_unaligned(v) }} }}"
                        ));
                    }
                    Ty::Rec(r) if a.differs(r) => {
                        m.push(format!("#[inline] pub fn {name}($R, i: usize) -> At<{elem}> {{ At(self.{name}_ptr(i)) }}"));
                    }
                    _ => {}
                }
            }
            _ => {
                a.warnings.push(format!(
                    "{rec}.{name} has a type the generator can't place ({}), no accessor for it",
                    tokens(ut)
                ));
            }
        }
    }
    let ubits = u.bitfields.get(rec).cloned().unwrap_or_default();
    for b in &ubits {
        let mut gets = Vec::new();
        let mut sets = Vec::new();
        let mut missing = false;
        for md in &models {
            let hb = md
                .bitfields
                .get(rec)
                .and_then(|v| v.iter().find(|x| x.name == b.name));
            let units = md.unit_offsets(rec);
            match hb.and_then(|hb| Some((hb, *units.get(&hb.unit)?))) {
                Some((hb, unit)) => {
                    gets.push((
                        md.host,
                        format!(
                            "ape::host::layout::get_bits(p.add({unit}), {}, {})",
                            hb.offset, hb.width
                        ),
                    ));
                    sets.push((
                        md.host,
                        format!(
                            "ape::host::layout::set_bits(p.add({unit}), {}, {}, v as u64)",
                            hb.offset, hb.width
                        ),
                    ));
                }
                None => missing = true,
            }
        }
        if missing {
            a.warnings.push(format!(
                "{rec}.{} is a bitfield not found on every host, no accessor for it",
                b.name
            ));
            continue;
        }
        let ret = tokens(&b.ret);
        let conv = if last_ident(&b.ret).as_deref() == Some("bool") {
            " != 0".to_string()
        } else {
            format!(" as {ret}")
        };
        let name = &b.name;
        let get = branch(&gets, &gets[0].1);
        let set = branch(&sets, &sets[0].1);
        m.push(format!(
            "#[inline] pub fn {name}($R) -> {ret} {{ let p = $P; unsafe {{ ({get}){conv} }} }}\n\
             #[inline] pub fn set_{name}($W, v: {ret}) {{ let p = $M; unsafe {{ {set} }} }}"
        ));
    }
    let body = m.join("\n");
    let own = body
        .replace("$R", "&self")
        .replace("$W", "&mut self")
        .replace("$PM", "(self as *const Self as *mut u8)")
        .replace("$P", "(self as *const Self as *const u8)")
        .replace("$M", "(self as *mut Self as *mut u8)");
    let at = body
        .replace("$R", "self")
        .replace("$W", "self")
        .replace("$PM", "(self.0 as *mut u8)")
        .replace("$P", "(self.0 as *const u8)")
        .replace("$M", "(self.0 as *mut u8)");
    let host_size = branch(
        &sizes
            .iter()
            .map(|(h, s)| (*h, format!("{}usize", s.0)))
            .collect::<Vec<_>>(),
        &format!("{us}usize"),
    );
    let where_ = sizes
        .iter()
        .map(|(h, s)| format!("{} bytes on {}", s.0, h.name()))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "/// Laid out as the host we run on lays it out, which differs between\n\
         /// hosts ({where_}): reach the fields through the methods, here or on\n\
         /// an [`At`] pointing at one, and step through an array of them with\n\
         /// [`{rec}::at`].\n\
         #[repr(C, align({align}))]\n#[derive(Copy, Clone)]\n\
         pub struct {rec} {{ _host: [u8; {size}] }}\n\
         impl Default for {rec} {{ fn default() -> Self {{ unsafe {{ ::core::mem::zeroed() }} }} }}\n\
         impl ::core::fmt::Debug for {rec} {{\n\
             fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {{\n\
                 f.debug_struct(\"{rec}\").finish_non_exhaustive()\n\
             }}\n\
         }}\n\
         #[allow(non_snake_case, unused_parens, clippy::missing_safety_doc)]\n\
         impl {rec} {{\n\
         /// Its size on the host we run on.\n\
         #[inline] pub fn host_size() -> usize {{ {host_size} }}\n\
         /// Element `i` of an array of them.\n\
         #[inline] pub unsafe fn at(p: *mut Self, i: usize) -> *mut Self {{ unsafe {{ (p as *mut u8).add(i * Self::host_size()) as *mut Self }} }}\n\
         {own}\n}}\n\
         #[allow(non_snake_case, unused_parens, clippy::missing_safety_doc)]\n\
         impl At<{rec}> {{\n{at}\n}}\n"
    )
}

// how libffi is told to pass `rec` by value, per host
pub(super) fn emit_ffi_kind(a: &mut Analysis, rec: &str) -> String {
    let arms: Vec<(Host, String)> = a
        .all()
        .iter()
        .filter_map(|m| {
            let list = |arm: bool| {
                m.ffi_members(rec, 0, arm)
                    .map(|v| format!("Struct(&[{}])", v.join(", ")))
            };
            let text = match m.host {
                Host::Win => list(false)?,
                Host::Xnu => list(true)?,
                Host::Linux => {
                    let (x86, arm) = (list(false)?, list(true)?);
                    if x86 == arm {
                        x86
                    } else {
                        format!("if cfg!(target_arch = \"aarch64\") {{ {arm} }} else {{ {x86} }}")
                    }
                }
            };
            Some((m.host, text))
        })
        .collect();
    let kind = branch(&arms, &arms[0].1);
    format!(
        "impl ape::host::ffi::FfiKind for {rec} {{\n\
             #[allow(unused_parens)]\n\
             fn kind() -> ape::host::ffi::Kind {{\n\
                 #[allow(unused_imports)]\n\
                 use ape::host::ffi::Kind::*;\n\
                 {kind}\n\
             }}\n\
         }}\n"
    )
}
