//! The crate's type items and what a Windows DLL would make of them:
//! callback fields, and structs laid out differently there.

use std::collections::{HashMap, HashSet};

use syn::spanned::Spanned;

use super::plan::{Plan, Scope};
use super::source::{Src, items_at, span_bytes, tokens};

// C types whose size a Windows DLL disagrees about: 4 bytes for the first
// two, 2 for the last
const MODEL: [(&str, &str); 3] = [
    ("c_long", "long"),
    ("c_ulong", "unsigned long"),
    ("wchar_t", "wchar_t"),
];

// what a declaration's signature says about the data model, see Types
#[derive(Default)]
pub(super) struct Signature {
    /// parameters that are pointers to a MODEL type, with the C name
    pub(super) pointers: Vec<(String, &'static str)>,
    /// a return of a MODEL type by value, which the host gives narrower
    pub(super) ret: Option<&'static str>,
    /// parameters that are callbacks taking or returning a MODEL type,
    /// with the marks for the descriptor (see model_bits)
    pub(super) callbacks: Vec<(usize, u64)>,
}

struct Alias {
    scope: usize,
    pub(super) vis: String,
    pub(super) ty: syn::Type,
    /// the item's bytes in its file, attributes included; a companion goes
    /// after the end
    pub(super) start: usize,
    pub(super) end: usize,
}

struct Struct {
    scope: usize,
    pub(super) ident: String,
    /// (name, type, type's byte range in the file)
    fields: Vec<(String, syn::Type, usize, usize)>,
    /// where the item starts, attributes included
    pub(super) start: usize,
}

/// The crate's type items, and what a Windows DLL would make of them.
///
/// A C function pointer in a struct field is one the DLL calls, or fills
/// in, with its own convention, which a value of the field's declared type
/// doesn't have. Such a field is retyped `HostCallback<F>`, so code that
/// assigns it has to say `HostCallback::new(f)`, which makes `f` callable
/// the DLL's way, and code that calls it goes through `get()`. A type
/// alias of such a pointer gets a companion `__ape_cb_<name>` alias of the
/// wrapped form, which fields of the alias's type are retyped to. A
/// struct with a field of a type the DLL sizes differently (MODEL, or a
/// struct that has one) is marked deprecated with a note saying so, and
/// so is an alias of it; a function with a pointer to such a type likewise.
pub(super) struct Types {
    /// by name, from every module; several when modules reuse a name
    aliases: HashMap<String, Vec<Alias>>,
    structs: Vec<Struct>,
    /// the names of all type items, for a clash with the helper's
    pub(super) names: HashSet<String>,
    /// (scope, name) of the structs and aliases laid out differently, with
    /// the reason
    pub(super) model: HashMap<(usize, String), String>,
    pub(super) scopes: Vec<Vec<String>>,
}

impl Types {
    pub(super) fn collect(files: &[Src], scopes: &[Scope], on_windows: bool) -> Types {
        let mut t = Types {
            aliases: HashMap::new(),
            structs: Vec::new(),
            names: HashSet::new(),
            model: HashMap::new(),
            scopes: scopes.iter().map(|s| s.modpath.clone()).collect(),
        };
        for (si, Scope { fi, inline, .. }) in scopes.iter().enumerate() {
            let src = &files[*fi];
            for item in items_at(&src.file.items, inline) {
                match item {
                    syn::Item::Type(a) => {
                        t.names.insert(a.ident.to_string());
                        if !a.generics.params.is_empty() {
                            continue;
                        }
                        let (start, end) = span_bytes(src, a.span(), &a.attrs);
                        t.aliases
                            .entry(a.ident.to_string())
                            .or_default()
                            .push(Alias {
                                scope: si,
                                vis: tokens(&a.vis),
                                ty: (*a.ty).clone(),
                                start,
                                end,
                            });
                    }
                    syn::Item::Struct(s) => {
                        t.names.insert(s.ident.to_string());
                        let (start, _) = span_bytes(src, s.span(), &s.attrs);
                        t.structs.push(Struct {
                            scope: si,
                            ident: s.ident.to_string(),
                            fields: fields_of(src, &s.fields),
                            start,
                        });
                    }
                    syn::Item::Union(u) => {
                        t.names.insert(u.ident.to_string());
                        let (start, _) = span_bytes(src, u.span(), &u.attrs);
                        let fields = syn::Fields::Named(u.fields.clone());
                        t.structs.push(Struct {
                            scope: si,
                            ident: u.ident.to_string(),
                            fields: fields_of(src, &fields),
                            start,
                        });
                    }
                    syn::Item::Enum(e) => {
                        t.names.insert(e.ident.to_string());
                    }
                    _ => {}
                }
            }
        }
        if on_windows {
            t.find_model();
        }
        t
    }

    // the alias a path names, from `scope`: the module's own first, then
    // the one whose module the path's leading segments fit
    fn alias(&self, scope: usize, path: &syn::Path) -> Option<&Alias> {
        let last = path.segments.last()?;
        if !last.arguments.is_none() {
            return None;
        }
        let list = self.aliases.get(&last.ident.to_string())?;
        if path.segments.len() == 1
            && let Some(a) = list.iter().find(|a| a.scope == scope)
        {
            return Some(a);
        }
        if list.len() == 1 {
            return Some(&list[0]);
        }
        let lead: Vec<String> = path
            .segments
            .iter()
            .take(path.segments.len() - 1)
            .map(|s| s.ident.to_string())
            .collect();
        let lead: Vec<&String> = lead
            .iter()
            .filter(|s| !matches!(s.as_str(), "crate" | "self" | "super"))
            .collect();
        list.iter().find(|a| {
            let mp = &self.scopes[a.scope];
            mp.len() >= lead.len()
                && mp[mp.len() - lead.len()..]
                    .iter()
                    .zip(&lead)
                    .all(|(x, y)| x == *y)
        })
    }

    fn companion_path(&self, a: &Alias, name: &str) -> String {
        let mp = &self.scopes[a.scope];
        if mp.is_empty() {
            format!("crate::__ape_cb_{name}")
        } else {
            format!("crate::{}::__ape_cb_{name}", mp.join("::"))
        }
    }

    // the type a callback field becomes, if it is one
    fn wrap(&self, scope: usize, ty: &syn::Type, depth: u32) -> Option<String> {
        if depth > 8 {
            return None;
        }
        match ty {
            syn::Type::FnPtr(f) => {
                let abi = f.abi.as_ref()?;
                let name = abi.name.as_ref().map_or("C".to_string(), |n| n.value());
                if !matches!(name.as_str(), "C" | "system" | "C-unwind" | "system-unwind") {
                    return None;
                }
                let m = self.model_bits(scope, f);
                if m != 0 {
                    Some(format!(
                        "crate::__ape_hostlink::HostCallback<{}, {m:#x}>",
                        tokens(ty)
                    ))
                } else {
                    Some(format!(
                        "crate::__ape_hostlink::HostCallback<{}>",
                        tokens(ty)
                    ))
                }
            }
            syn::Type::Array(a) => {
                let w = self.wrap(scope, &a.elem, depth + 1)?;
                Some(format!("[{w}; {}]", tokens(&a.len)))
            }
            syn::Type::Path(p) => {
                let last = p.path.segments.last()?;
                if last.ident == "Option"
                    && let syn::PathArguments::AngleBracketed(g) = &last.arguments
                    && g.args.len() == 1
                    && let Some(syn::GenericArgument::Type(inner)) = g.args.first()
                {
                    let w = self.wrap(scope, inner, depth + 1)?;
                    return Some(format!("Option<{w}>"));
                }
                let a = self.alias(scope, &p.path)?;
                self.wrap(a.scope, &a.ty, depth + 1)?;
                Some(self.companion_path(a, &last.ident.to_string()))
            }
            _ => None,
        }
    }

    // the C name of a MODEL type this type is, through aliases
    fn model_of(&self, scope: usize, ty: &syn::Type, depth: u32) -> Option<&'static str> {
        if depth > 8 {
            return None;
        }
        match ty {
            syn::Type::Array(a) => self.model_of(scope, &a.elem, depth + 1),
            syn::Type::Path(p) => {
                let last = p.path.segments.last()?;
                if last.arguments.is_none()
                    && let Some((_, c)) = MODEL.iter().find(|(r, _)| last.ident == r)
                {
                    return Some(c);
                }
                let a = self.alias(scope, &p.path)?;
                self.model_of(a.scope, &a.ty, depth + 1)
            }
            _ => None,
        }
    }

    // why a field's type is laid out differently, if it is: itself a
    // MODEL type, or a struct or alias already found to be
    fn model_field(&self, scope: usize, ty: &syn::Type, depth: u32) -> Option<String> {
        if let Some(c) = self.model_of(scope, ty, 0) {
            let bytes = if c == "wchar_t" { 2 } else { 4 };
            return Some(format!("a C `{c}`, {bytes} bytes there"));
        }
        self.model_struct(scope, ty, depth)
    }

    // the struct part of model_field: an alias to a `long` is only a
    // `long`, but an alias to such a struct is that struct
    fn model_struct(&self, scope: usize, ty: &syn::Type, depth: u32) -> Option<String> {
        if depth > 8 {
            return None;
        }
        match ty {
            syn::Type::Array(a) => self.model_struct(scope, &a.elem, depth + 1),
            syn::Type::Path(p) => {
                let last = p.path.segments.last()?;
                if !last.arguments.is_none() {
                    return None;
                }
                let name = last.ident.to_string();
                let found = |s: usize| self.model.contains_key(&(s, name.clone()));
                let hit = if path_is_local(&p.path) && found(scope) {
                    true
                } else if let Some(a) = self.alias(scope, &p.path) {
                    found(a.scope)
                } else {
                    let mut hits = self.model.keys().filter(|(_, n)| *n == name);
                    hits.next().is_some() && hits.next().is_none()
                };
                hit.then(|| format!("a `{name}`, which is laid out differently"))
            }
            _ => None,
        }
    }

    // a struct with a field laid out differently is itself, and so is an
    // alias of it, until nothing new turns up
    fn find_model(&mut self) {
        loop {
            let mut found: Vec<((usize, String), String)> = Vec::new();
            for s in &self.structs {
                let key = (s.scope, s.ident.clone());
                if self.model.contains_key(&key) {
                    continue;
                }
                if let Some((name, why)) = s
                    .fields
                    .iter()
                    .find_map(|(n, ty, _, _)| self.model_field(s.scope, ty, 0).map(|w| (n, w)))
                {
                    found.push((key, format!("field `{name}` is {why}")));
                }
            }
            for (name, list) in &self.aliases {
                for a in list {
                    let key = (a.scope, name.clone());
                    if self.model.contains_key(&key) {
                        continue;
                    }
                    if let Some(why) = self.model_struct(a.scope, &a.ty, 0) {
                        found.push((key, format!("it is {why}")));
                    }
                }
            }
            if found.is_empty() {
                break;
            }
            self.model.extend(found);
        }
    }

    pub(super) fn signature(&self, scope: usize, sig: &syn::Signature) -> Signature {
        let mut out = Signature::default();
        for (i, a) in sig.inputs.iter().enumerate() {
            let syn::FnArg::Typed(t) = a else { continue };
            if let syn::Type::Ptr(p) = &*t.ty
                && let Some(c) = self.model_of(scope, &p.elem, 0)
            {
                out.pointers.push((tokens(&t.pat), c));
            }
            if let Some(f) = self.fn_ptr(scope, &t.ty, 0) {
                let m = self.model_bits(scope, f);
                if m != 0 {
                    out.callbacks.push((i, m));
                }
            }
        }
        if let syn::ReturnType::Type(_, t) = &sig.output
            && let Some(c) = self.model_of(scope, t, 0)
        {
            out.ret = Some(c);
        }
        out
    }

    // the C fn pointer type `ty` is, through Option and aliases
    fn fn_ptr<'t>(
        &'t self,
        scope: usize,
        ty: &'t syn::Type,
        depth: u32,
    ) -> Option<&'t syn::TypeFnPtr> {
        if depth > 8 {
            return None;
        }
        match ty {
            syn::Type::FnPtr(f) => Some(f),
            syn::Type::Path(p) => {
                let last = p.path.segments.last()?;
                if last.ident == "Option"
                    && let syn::PathArguments::AngleBracketed(g) = &last.arguments
                    && g.args.len() == 1
                    && let Some(syn::GenericArgument::Type(inner)) = g.args.first()
                {
                    return self.fn_ptr(scope, inner, depth + 1);
                }
                let a = self.alias(scope, &p.path)?;
                self.fn_ptr(a.scope, &a.ty, depth + 1)
            }
            _ => None,
        }
    }

    // the marks a callback's descriptor carries for the parameters and
    // return value a Windows DLL passes narrower: two bits per parameter
    // from bit 40 for the first eleven (1 a `long`, 2 an `unsigned long`,
    // 3 a `wchar_t`), the return value's in bits 62 and 63; see
    // __hostlink_nt2sysv_layout in the fork
    fn model_bits(&self, scope: usize, f: &syn::TypeFnPtr) -> u64 {
        let mark = |ty: &syn::Type| -> u64 {
            match self.model_of(scope, ty, 0) {
                Some("long") => 1,
                Some("unsigned long") => 2,
                Some("wchar_t") => 3,
                _ => 0,
            }
        };
        let mut m = 0;
        for (i, a) in f.inputs.iter().enumerate().take(11) {
            m |= mark(&a.ty) << (40 + 2 * i);
        }
        if let syn::ReturnType::Type(_, t) = &f.output {
            m |= mark(t) << 62;
        }
        m
    }

    pub(super) fn rewrite(&self, scopes: &[Scope], plan: &mut Plan) {
        for (name, list) in &self.aliases {
            for a in list {
                let fi = scopes[a.scope].fi;
                if let Some(w) = self.wrap(a.scope, &a.ty, 0) {
                    plan.edits.push((
                        fi,
                        a.end,
                        a.end,
                        format!(" #[doc(hidden)] {} type __ape_cb_{name} = {w};", a.vis),
                    ));
                }
                if let Some(why) = self.model.get(&(a.scope, name.clone())) {
                    plan.edits.push((
                        fi,
                        a.start,
                        a.start,
                        format!("#[deprecated(note = {})] ", model_note(name, why)),
                    ));
                }
            }
        }
        for s in &self.structs {
            let fi = scopes[s.scope].fi;
            for (_, ty, start, end) in &s.fields {
                if let Some(w) = self.wrap(s.scope, ty, 0) {
                    plan.edits.push((fi, *start, *end, w));
                    plan.callbacks = true;
                }
            }
            if let Some(why) = self.model.get(&(s.scope, s.ident.clone())) {
                plan.edits.push((
                    fi,
                    s.start,
                    s.start,
                    format!("#[deprecated(note = {})] ", model_note(&s.ident, why)),
                ));
            }
        }
    }
}

// a path of one segment, or one starting at `self`
fn path_is_local(p: &syn::Path) -> bool {
    p.segments.len() == 1 || (p.segments.len() == 2 && p.segments[0].ident == "self")
}

fn fields_of(src: &Src, fields: &syn::Fields) -> Vec<(String, syn::Type, usize, usize)> {
    fields
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let r = f.ty.span().byte_range();
            let name = f
                .ident
                .as_ref()
                .map_or_else(|| i.to_string(), |i| i.to_string());
            (name, f.ty.clone(), src.base + r.start, src.base + r.end)
        })
        .collect()
}

fn model_note(name: &str, why: &str) -> String {
    format!(
        "{:?}",
        format!(
            "rust-ape: a Windows DLL lays `{name}` out differently, {why}. A struct the host \
reads needs a binding made for its data model; see README.md, the host data model"
        )
    )
}
