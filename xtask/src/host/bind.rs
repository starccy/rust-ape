//! `cargo xtask host-bindings`: turns C declarations meant for static linking
//! (bindgen output, or the `extern "C"` blocks of a -sys crate) into the
//! `#[ape::host_library]` form, which loads the library at runtime on whatever
//! host the binary lands on.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use quote::ToTokens;
use quote::quote;
use syn::visit_mut::VisitMut;

/// What the command was asked to do; cli/host_bindings.rs has the flags.
pub struct Options {
    /// bindgen output, or any Rust file with `extern "C"` blocks
    pub input: PathBuf,
    /// Where to write the result (default: stdout)
    pub out: Option<PathBuf>,
    /// Name of the static that loads the library
    pub name: String,
    /// Library names to try on Linux, in order (repeatable)
    pub linux: Vec<String>,
    /// Library names to try on macOS
    pub macos: Vec<String>,
    /// DLL names to try on Windows
    pub windows: Vec<String>,
    /// Library names to try on the BSDs
    pub bsd: Vec<String>,
    /// Leave function pointer types alone instead of making them
    /// `ape::host::Callback`
    pub keep_fn_types: bool,
    /// bindgen output for the same header made with
    /// `--target=x86_64-pc-windows-msvc`: structures and functions whose
    /// types differ there (`long`, `wchar_t`, `long double`) are made to
    /// follow the host at runtime, see ape::host::layout
    pub windows_input: Option<PathBuf>,
    /// bindgen output for the same header made with
    /// `--target=aarch64-apple-macos`, where `long double` is a `double`
    pub macos_input: Option<PathBuf>,
    /// With --windows-input: parameter `p` of function `f` points at as
    /// many `long`s or `wchar_t`s as parameter `n` says (repeatable)
    pub len: Vec<String>,
    /// Go ahead without the per-host inputs a header using `long`,
    /// `wchar_t` or `long double` needs, and declare it as on Linux
    pub ignore_missing_hosts: bool,
}

// `Option<unsafe extern "C" fn(..)>` and bare `extern "C" fn(..)` become
// ape::host::Callback, since a host library calls back with its own
// convention, which on Windows isn't the one an extern "C" fn has
struct Callbacks {
    count: usize,
}

fn c_abi(name: &str) -> bool {
    matches!(name, "C" | "system" | "C-unwind" | "system-unwind")
}

fn is_c_fn(ty: &syn::Type) -> bool {
    match ty {
        syn::Type::FnPtr(f) => f
            .abi
            .as_ref()
            .is_some_and(|abi| abi.name.as_ref().is_none_or(|n| c_abi(&n.value()))),
        _ => false,
    }
}

// `long` is 32 bits on Windows and 64 elsewhere, and a `long double` (a
// `u128` to bindgen on Linux) 8 bytes on Windows and macOS, so a
// declaration with one is right on one host and wrong on another. Aliases
// of them count, `pub type uLong = c_ulong` and on
#[derive(Default)]
struct Widths {
    longs: Vec<String>,
    doubles: Vec<String>,
}

fn idents(tokens: proc_macro2::TokenStream) -> Vec<String> {
    tokens
        .to_string()
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect()
}

impl Widths {
    fn scan(file: &syn::File) -> Widths {
        let mut longs: std::collections::HashSet<String> =
            ["c_long", "c_ulong", "wchar_t"].map(String::from).into();
        let mut doubles: std::collections::HashSet<String> = ["u128"].map(String::from).into();
        let aliases: Vec<(String, Vec<String>)> = file
            .items
            .iter()
            .filter_map(|i| match i {
                syn::Item::Type(t) => Some((t.ident.to_string(), idents(quote!(#t)))),
                _ => None,
            })
            .collect();
        loop {
            let mut grew = false;
            for (name, used) in &aliases {
                if !longs.contains(name) && used.iter().any(|u| longs.contains(u)) {
                    longs.insert(name.clone());
                    grew = true;
                }
                if !doubles.contains(name) && used.iter().any(|u| doubles.contains(u)) {
                    doubles.insert(name.clone());
                    grew = true;
                }
            }
            if !grew {
                break;
            }
        }
        let mut w = Widths::default();
        let mut check = |name: String, used: Vec<String>| {
            if used.iter().any(|u| longs.contains(u)) {
                w.longs.push(name.clone());
            }
            if used.iter().any(|u| doubles.contains(u)) {
                w.doubles.push(name);
            }
        };
        for item in &file.items {
            match item {
                syn::Item::Struct(s) => check(s.ident.to_string(), idents(quote!(#s))),
                syn::Item::Union(u) => check(u.ident.to_string(), idents(quote!(#u))),
                syn::Item::ForeignMod(b) => {
                    for fi in &b.items {
                        if let syn::ForeignItem::Fn(f) = fi {
                            check(f.sig.ident.to_string(), idents(quote!(#f)));
                        }
                    }
                }
                _ => {}
            }
        }
        w
    }
}

fn option_of_c_fn(ty: &syn::Type) -> bool {
    let syn::Type::Path(p) = ty else { return false };
    let Some(last) = p.path.segments.last() else {
        return false;
    };
    if last.ident != "Option" {
        return false;
    }
    let syn::PathArguments::AngleBracketed(args) = &last.arguments else {
        return false;
    };
    matches!(args.args.first(), Some(syn::GenericArgument::Type(t)) if is_c_fn(t))
}

impl VisitMut for Callbacks {
    fn visit_type_mut(&mut self, ty: &mut syn::Type) {
        if is_c_fn(ty) || option_of_c_fn(ty) {
            *ty = syn::parse_quote!(ape::host::Callback);
            self.count += 1;
            return;
        }
        syn::visit_mut::visit_type_mut(self, ty);
    }
}

// a `va_list` parameter is a pointer-sized argument on every host, to
// different things: `*mut __va_list_tag` here, `*mut c_char` on Windows
// and Apple Silicon. It becomes `*mut c_void`, for ape::host::VaList,
// and the tag type goes
struct VaLists {
    count: usize,
}

fn va_list_name(ty: &syn::Type) -> bool {
    let syn::Type::Path(p) = ty else { return false };
    p.path.segments.last().is_some_and(|s| {
        matches!(
            s.ident.to_string().as_str(),
            "va_list" | "__gnuc_va_list" | "__builtin_va_list" | "__va_list_tag"
        )
    })
}

impl VisitMut for VaLists {
    fn visit_type_mut(&mut self, ty: &mut syn::Type) {
        let hit = match ty {
            syn::Type::Ptr(p) => va_list_name(&p.elem),
            _ => va_list_name(ty),
        };
        if hit {
            *ty = syn::parse_quote!(*mut ::std::os::raw::c_void);
            self.count += 1;
            return;
        }
        syn::visit_mut::visit_type_mut(self, ty);
    }
}

fn strip_va_list_items(file: &mut syn::File) {
    file.items.retain(|item| {
        let name = match item {
            syn::Item::Struct(s) => s.ident.to_string(),
            syn::Item::Type(t) => t.ident.to_string(),
            // bindgen's layout tests for it
            syn::Item::Const(c) => {
                return !c.to_token_stream().to_string().contains("__va_list_tag");
            }
            _ => return true,
        };
        !matches!(
            name.as_str(),
            "va_list" | "__gnuc_va_list" | "__builtin_va_list" | "__va_list_tag"
        )
    });
}

pub fn run(args: &Options) -> Result<()> {
    let src = std::fs::read_to_string(&args.input)
        .with_context(|| format!("reading {}", args.input.display()))?;
    let mut file = syn::parse_file(&src).context("parsing the input as Rust")?;
    let mut dual = None;
    let mut callback_warnings = Vec::new();
    let mut callback_recs = std::collections::BTreeSet::new();
    let read = |path: &Option<PathBuf>, what: &str| -> Result<Option<syn::File>> {
        let Some(p) = path else { return Ok(None) };
        let src = std::fs::read_to_string(p).with_context(|| format!("reading {}", p.display()))?;
        Ok(Some(syn::parse_file(&src).with_context(|| {
            format!("parsing the {what} input as Rust")
        })?))
    };
    let mut wfile = read(&args.windows_input, "Windows")?;
    let mut xfile = read(&args.macos_input, "macOS")?;
    let widths = Widths::scan(&file);
    if !args.ignore_missing_hosts {
        if wfile.is_none() && !widths.longs.is_empty() {
            bail!(
                "`long` or `wchar_t` is used by {}, which Windows sizes differently: give --windows-input \
                 (bindgen with --target=x86_64-pc-windows-msvc), or --ignore-missing-hosts to declare them as on Linux",
                widths.longs.join(", ")
            );
        }
        if (wfile.is_none() || xfile.is_none()) && !widths.doubles.is_empty() {
            bail!(
                "a u128, likely a `long double`, is used by {}, which Windows and macOS make a double: give both \
                 --windows-input and --macos-input (bindgen with --target=aarch64-apple-macos), or --ignore-missing-hosts",
                widths.doubles.join(", ")
            );
        }
    }
    if wfile.is_some() || xfile.is_some() {
        (callback_warnings, callback_recs) =
            crate::host::layout::callback_warnings(&file, wfile.as_ref(), xfile.as_ref());
    }
    let mut va_lists = VaLists { count: 0 };
    va_lists.visit_file_mut(&mut file);
    strip_va_list_items(&mut file);
    for f in wfile.iter_mut().chain(xfile.iter_mut()) {
        VaLists { count: 0 }.visit_file_mut(f);
        strip_va_list_items(f);
    }
    let mut callbacks = Callbacks { count: 0 };
    if !args.keep_fn_types {
        callbacks.visit_file_mut(&mut file);
        for f in wfile.iter_mut().chain(xfile.iter_mut()) {
            Callbacks { count: 0 }.visit_file_mut(f);
        }
    }
    if wfile.is_some() || xfile.is_some() {
        let mut lens = std::collections::HashMap::new();
        for l in &args.len {
            let (f, rest) = l
                .split_once(':')
                .with_context(|| format!("--len {l}: want f:p=n"))?;
            let (p, n) = rest
                .split_once('=')
                .with_context(|| format!("--len {l}: want f:p=n"))?;
            lens.insert((f.to_string(), p.to_string()), n.to_string());
        }
        let d = crate::host::layout::analyze(
            &file,
            wfile.as_ref(),
            xfile.as_ref(),
            &lens,
            &callback_recs,
        )?;
        crate::host::layout::strip(&mut file, &d);
        dual = Some(d);
    }

    let mut kept = Vec::new();
    let mut fns = Vec::new();
    let mut variadics = Vec::new();
    let mut nested = Vec::new();
    for item in file.items {
        let syn::Item::ForeignMod(block) = item else {
            if let syn::Item::Mod(m) = &item {
                if m.content.as_ref().is_some_and(|(_, items)| {
                    items.iter().any(|i| matches!(i, syn::Item::ForeignMod(_)))
                }) {
                    nested.push(m.ident.to_string());
                }
            }
            kept.push(item);
            continue;
        };
        if block.abi.name.as_ref().is_some_and(|n| !c_abi(&n.value())) {
            bail!("only extern \"C\" blocks can be converted");
        }
        for item in block.items {
            match item {
                syn::ForeignItem::Fn(mut f) => {
                    // the macro wants link_name first, then doc comments
                    let (link, rest): (Vec<_>, Vec<_>) = f
                        .attrs
                        .drain(..)
                        .partition(|a| a.path().is_ident("link_name"));
                    f.attrs = link;
                    f.attrs
                        .extend(rest.into_iter().filter(|a| a.path().is_ident("doc")));
                    // and #[host] before that
                    if let Some(d) = &dual {
                        crate::host::layout::declare_long_doubles(&mut f, d);
                        if let Some(a) = d.sigs.get(&f.sig.ident.to_string()) {
                            f.attrs.insert(0, a.clone());
                        }
                    }
                    if f.sig.variadic.is_some() {
                        variadics.push(f);
                    } else {
                        fns.push(syn::ForeignItem::Fn(f));
                    }
                }
                syn::ForeignItem::Static(mut s) => {
                    s.attrs.retain(|a| a.path().is_ident("doc"));
                    fns.push(syn::ForeignItem::Static(s));
                }
                other => kept.push(syn::Item::Verbatim(quote!(#other))),
            }
        }
    }

    let nvariadic = variadics.len();
    if let Some(d) = &dual {
        kept.extend(d.items.iter().cloned());
    }

    // the block the attribute goes on, variadic functions included
    let block: syn::ItemForeignMod = syn::parse_quote! {
        unsafe extern "C" {
            #(#fns)*
            #(#variadics)*
        }
    };
    let decls = prettyplease::unparse(&syn::File {
        shebang: None,
        frontmatter: None,
        attrs: vec![],
        items: vec![syn::Item::ForeignMod(block)],
    });
    let list = |v: &[String]| {
        v.iter()
            .map(|s| format!("{s:?}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let rest = prettyplease::unparse(&syn::File {
        shebang: None,
        frontmatter: None,
        attrs: file.attrs,
        items: kept,
    });
    let mut out = String::new();
    out.push_str("// generated by `cargo xtask host-bindings`, see ape::host_library\n\n");
    out.push_str(&rest);
    let hosts = [
        ("linux", &args.linux),
        ("macos", &args.macos),
        ("windows", &args.windows),
        ("bsd", &args.bsd),
    ]
    .into_iter()
    .filter(|(_, names)| !names.is_empty())
    .map(|(os, names)| format!("{os} [{}]", list(names)))
    .collect::<Vec<_>>();
    out.push_str(&format!(
        "\n#[ape::host_library(pub static {} = {})]\n",
        args.name,
        hosts.join(",\n    ")
    ));
    out.push_str(&decls);
    let out = rustfmt(out);
    if let Some(path) = &args.out {
        std::fs::write(path, out).with_context(|| format!("writing {}", path.display()))?;
    } else {
        print!("{out}");
    }
    eprintln!(
        "{} functions, {} variadic, {} callback types made ape::host::Callback",
        fns.len(),
        nvariadic,
        callbacks.count
    );
    if va_lists.count > 0 {
        eprintln!(
            "{} va_list parameters made *mut c_void, for ape::host::VaList",
            va_lists.count
        );
    }
    if let Some(d) = &dual {
        eprintln!(
            "{} structures follow the host's layout, {} functions convert on some host",
            d.converted.len(),
            d.sigs.len()
        );
        for w in d.warnings.iter().chain(&callback_warnings) {
            eprintln!("  {w}");
        }
    } else if !widths.longs.is_empty() || !widths.doubles.is_empty() {
        eprintln!(
            "mind that `long`, `wchar_t` and `long double` are sized differently on Windows and macOS: {} use them, \
             and are declared as on Linux",
            widths
                .longs
                .iter()
                .chain(&widths.doubles)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    if !nested.is_empty() {
        eprintln!(
            "left alone, extern blocks inside modules: {}",
            nested.join(", ")
        );
    }
    Ok(())
}

/// `src` through rustfmt, or unchanged when rustfmt isn't there or fails.
fn rustfmt(src: String) -> String {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let Ok(mut child) = Command::new("rustfmt")
        .args(["--edition", "2024", "--emit", "stdout"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return src;
    };
    let mut stdin = child.stdin.take().unwrap();
    let input = src.clone();
    let writer = std::thread::spawn(move || stdin.write_all(input.as_bytes()));
    let out = child.wait_with_output();
    let _ = writer.join();
    match out {
        Ok(o) if o.status.success() => String::from_utf8(o.stdout).unwrap_or(src),
        _ => src,
    }
}
