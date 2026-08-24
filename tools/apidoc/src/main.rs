// SPDX-License-Identifier: Apache-2.0

//! Generate the flintos.dev API reference as Starlight pages from rustdoc JSON.
//!
//! # Why generate, and why into the site
//!
//! Stock rustdoc is a separate, unthemeable site: it reads as a different
//! product from flintos.dev and its search is its own. Hand-writing the
//! reference instead would let it drift from the code -- the exact failure
//! #123 fought. So the reference is *generated from the source* (rustdoc's
//! `--output-format json`, which cannot drift) but rendered as ordinary
//! Starlight content: the site's fonts and theme for free, and -- the real
//! win -- every item indexed by the site's own Pagefind search.
//!
//! # Input
//!
//! One `<crate>.json` per crate, as `make apidoc` emits with
//! `RUSTC_BOOTSTRAP=1 cargo rustdoc -- --output-format json`. The JSON format
//! is versioned; this tool pins `rustdoc-types` to the matching release
//! (FORMAT_VERSION 57) so a toolchain bump that changes the shape is a
//! deliberate, paired change, not a silent mis-parse.
//!
//! # Output
//!
//! A tree of Markdown pages under the output dir -- one per module, named by
//! the module path -- plus `sidebar.json`, the generated `API` sidebar group
//! the Starlight config splices in. Both are build artifacts: generated, never
//! committed.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use rustdoc_types::{
    AssocItemConstraint, AssocItemConstraintKind, Crate, Enum, GenericArg, GenericArgs,
    GenericBound, Id, Item, ItemEnum, Struct, StructKind, Term, Trait, Type, Variant, VariantKind,
    WherePredicate,
};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 3 {
        eprintln!("usage: apidoc <json-dir> <out-dir>");
        eprintln!("  <json-dir>  directory of <crate>.json files (rustdoc --output-format json)");
        eprintln!("  <out-dir>   where the generated Starlight .md pages are written");
        std::process::exit(2);
    }
    let json_dir = PathBuf::from(&args[1]);
    let out_dir = PathBuf::from(&args[2]);

    let mut json_files: Vec<PathBuf> = fs::read_dir(&json_dir)
        .unwrap_or_else(|e| fatal(&format!("cannot read {}: {e}", json_dir.display())))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().map(|x| x == "json").unwrap_or(false))
        .collect();
    json_files.sort();
    if json_files.is_empty() {
        fatal(&format!("no *.json under {}", json_dir.display()));
    }

    // Clean the output dir so a removed crate/module leaves no stale page.
    if out_dir.exists() {
        fs::remove_dir_all(&out_dir).ok();
    }
    fs::create_dir_all(&out_dir).unwrap_or_else(|e| fatal(&format!("mkdir out: {e}")));

    let mut crates: Vec<CrateDoc> = Vec::new();
    for path in &json_files {
        let text = fs::read_to_string(path)
            .unwrap_or_else(|e| fatal(&format!("read {}: {e}", path.display())));
        let krate: Crate = serde_json::from_str(&text).unwrap_or_else(|e| {
            fatal(&format!(
                "parse {} (format mismatch? this tool pins FORMAT_VERSION 57): {e}",
                path.display()
            ))
        });
        crates.push(CrateDoc::render(krate, &out_dir));
    }

    // The generated sidebar group: one collapsed entry per crate, its modules
    // nested. Starlight's config reads this and splices it under "API".
    let sidebar = build_sidebar(&crates);
    let sidebar_path = out_dir.join("_sidebar.json");
    fs::write(&sidebar_path, sidebar).unwrap_or_else(|e| fatal(&format!("write sidebar: {e}")));

    write_landing(&out_dir, &crates);

    let pages: usize = crates.iter().map(|c| c.pages).sum();
    println!(
        "apidoc: {} page(s) across {} crate(s) -> {}",
        pages,
        crates.len(),
        out_dir.display()
    );
}

fn fatal(msg: &str) -> ! {
    eprintln!("apidoc: error: {msg}");
    std::process::exit(1);
}

/// What a rendered crate contributes to the sidebar, and how many pages it wrote.
struct CrateDoc {
    name: String,
    /// First line of the crate's root docs, for the landing page.
    summary: String,
    /// (module path segments, slug-relative-url) for every page written.
    modules: Vec<(Vec<String>, String)>,
    pages: usize,
}

impl CrateDoc {
    fn render(krate: Crate, out_dir: &Path) -> CrateDoc {
        let root = krate.index.get(&krate.root);
        let crate_name = root
            .and_then(|i| i.name.clone())
            .unwrap_or_else(|| "crate".to_string());
        let summary = first_line(root.and_then(|i| i.docs.as_deref()));

        let ctx = Ctx { krate: &krate };
        let mut modules = Vec::new();
        let mut pages = 0usize;

        // Walk the module tree depth-first from the crate root, emitting one
        // page per module. `path` accumulates the human module path
        // (crate::mod::sub) which is both the page title and its slug.
        let mut stack: Vec<(Id, Vec<String>)> = vec![(krate.root.clone(), vec![crate_name.clone()])];
        while let Some((id, path)) = stack.pop() {
            let Some(item) = krate.index.get(&id) else { continue };
            let ItemEnum::Module(module) = &item.inner else { continue };

            let page = ctx.render_module(&path, item, module);
            let slug = slug_for(&path);
            let file = out_dir.join(format!("{slug}.md"));
            if let Some(parent) = file.parent() {
                fs::create_dir_all(parent).ok();
            }
            fs::write(&file, page).unwrap_or_else(|e| fatal(&format!("write page: {e}")));
            modules.push((path.clone(), slug));
            pages += 1;

            // Queue child modules.
            for child_id in &module.items {
                if let Some(child) = krate.index.get(child_id) {
                    if let ItemEnum::Module(_) = &child.inner {
                        let name = child.name.clone().unwrap_or_default();
                        let mut cp = path.clone();
                        cp.push(name);
                        stack.push((child_id.clone(), cp));
                    }
                }
            }
        }

        modules.sort_by(|a, b| a.0.cmp(&b.0));
        CrateDoc {
            name: crate_name,
            summary,
            modules,
            pages,
        }
    }
}

/// The `/api/` landing page: one line per crate, linking its root module, so the
/// section has a front door rather than 404-ing before a crate is chosen.
fn write_landing(out_dir: &Path, crates: &[CrateDoc]) {
    let mut s = String::new();
    s.push_str("---\ntitle: \"API Reference\"\n---\n\n");
    s.push_str(
        "The full, generated API for every crate in the workspace. Regenerated \
         from the source on each build (`make apidoc`), so it cannot drift from \
         the code, and indexed by this site's search.\n\n",
    );
    s.push_str("## Crates\n\n");
    let mut sorted: Vec<&CrateDoc> = crates.iter().collect();
    sorted.sort_by(|a, b| a.name.cmp(&b.name));
    for c in sorted {
        let url = url_for(&[c.name.clone()]);
        if c.summary.is_empty() {
            let _ = writeln!(s, "- [`{}`]({})", c.name, url);
        } else {
            let _ = writeln!(s, "- [`{}`]({}) — {}", c.name, url, c.summary);
        }
    }
    fs::write(out_dir.join("index.md"), s).unwrap_or_else(|e| fatal(&format!("write landing: {e}")));
}

/// Shared read-only view over one crate while rendering it.
struct Ctx<'a> {
    krate: &'a Crate,
}

impl<'a> Ctx<'a> {
    /// Render one module page: frontmatter, module docs, then a section per
    /// item kind. Child *modules* are linked, not inlined.
    fn render_module(&self, path: &[String], item: &Item, module: &rustdoc_types::Module) -> String {
        let title = path.join("::");
        let mut out = String::new();
        // Starlight frontmatter. `title` shows in nav and search; the code
        // font makes the module path read as an identifier, not prose.
        let _ = writeln!(out, "---");
        let _ = writeln!(out, "title: \"{}\"", yaml_escape(&title));
        let _ = writeln!(out, "tableOfContents:");
        let _ = writeln!(out, "  minHeadingLevel: 2");
        let _ = writeln!(out, "  maxHeadingLevel: 2");
        let _ = writeln!(out, "---");
        out.push('\n');

        if let Some(docs) = &item.docs {
            out.push_str(docs);
            out.push_str("\n\n");
        }

        // Partition this module's direct children by kind.
        let mut submodules: Vec<&Item> = Vec::new();
        let mut structs: Vec<&Item> = Vec::new();
        let mut enums: Vec<&Item> = Vec::new();
        let mut traits: Vec<&Item> = Vec::new();
        let mut funcs: Vec<&Item> = Vec::new();
        let mut macros: Vec<&Item> = Vec::new();
        let mut consts: Vec<&Item> = Vec::new();
        let mut aliases: Vec<&Item> = Vec::new();

        for id in &module.items {
            let Some(child) = self.krate.index.get(id) else { continue };
            match &child.inner {
                ItemEnum::Module(_) => submodules.push(child),
                ItemEnum::Struct(_) => structs.push(child),
                ItemEnum::Enum(_) => enums.push(child),
                ItemEnum::Trait(_) => traits.push(child),
                ItemEnum::Function(_) => funcs.push(child),
                ItemEnum::Macro(_) => macros.push(child),
                ItemEnum::Constant { .. } => consts.push(child),
                ItemEnum::TypeAlias(_) => aliases.push(child),
                _ => {}
            }
        }
        for v in [
            &mut submodules,
            &mut structs,
            &mut enums,
            &mut traits,
            &mut funcs,
            &mut macros,
            &mut consts,
            &mut aliases,
        ] {
            v.sort_by(|a, b| a.name.cmp(&b.name));
        }

        if !submodules.is_empty() {
            let _ = writeln!(out, "## Modules\n");
            for m in &submodules {
                let name = m.name.clone().unwrap_or_default();
                let mut child_path = path.to_vec();
                child_path.push(name.clone());
                let url = url_for(&child_path);
                let summary = first_line(m.docs.as_deref());
                let _ = writeln!(out, "- [`{name}`]({url}) — {summary}");
            }
            out.push('\n');
        }

        self.section(&mut out, "Structs", &structs);
        self.section(&mut out, "Enums", &enums);
        self.section(&mut out, "Traits", &traits);
        self.section(&mut out, "Functions", &funcs);
        self.section(&mut out, "Macros", &macros);
        self.section(&mut out, "Constants", &consts);
        self.section(&mut out, "Type Aliases", &aliases);

        out
    }

    /// One "## Kind" section: each item gets a `### name` heading, its signature
    /// in a Rust code block, its docs, and any related items.
    fn section(&self, out: &mut String, heading: &str, items: &[&Item]) {
        if items.is_empty() {
            return;
        }
        let _ = writeln!(out, "## {heading}\n");
        for item in items {
            let name = item.name.clone().unwrap_or_default();
            let _ = writeln!(out, "### {name}\n");
            let sig = self.signature(item);
            if !sig.is_empty() {
                let _ = writeln!(out, "```rust\n{sig}\n```\n");
            }
            if let Some(docs) = &item.docs {
                out.push_str(docs);
                out.push_str("\n\n");
            }
            self.related(out, item);
        }
    }

    /// The item's declaration line(s). Enough to be a reference: full for
    /// functions/consts/aliases; a header plus fields/variants for
    /// structs/enums; a header plus required items for traits.
    fn signature(&self, item: &Item) -> String {
        let name = item.name.clone().unwrap_or_default();
        match &item.inner {
            ItemEnum::Function(f) => {
                let mut s = String::new();
                if f.header.is_const {
                    s.push_str("const ");
                }
                if f.header.is_async {
                    s.push_str("async ");
                }
                if f.header.is_unsafe {
                    s.push_str("unsafe ");
                }
                s.push_str("fn ");
                s.push_str(&name);
                s.push_str(&self.render_generics(&f.generics));
                s.push('(');
                let params: Vec<String> = f
                    .sig
                    .inputs
                    .iter()
                    .map(|(pname, ty)| {
                        if pname == "self" {
                            self.render_self(ty)
                        } else {
                            format!("{pname}: {}", self.ty(ty))
                        }
                    })
                    .collect();
                s.push_str(&params.join(", "));
                s.push(')');
                if let Some(out_ty) = &f.sig.output {
                    let _ = write!(s, " -> {}", self.ty(out_ty));
                }
                s.push_str(&self.render_where(&f.generics));
                s
            }
            ItemEnum::Struct(st) => self.struct_sig(&name, st),
            ItemEnum::Enum(en) => self.enum_sig(&name, en),
            ItemEnum::Trait(tr) => self.trait_sig(&name, tr),
            ItemEnum::TypeAlias(a) => {
                format!(
                    "type {name}{} = {};",
                    self.render_generics(&a.generics),
                    self.ty(&a.type_)
                )
            }
            ItemEnum::Constant { type_, .. } => {
                format!("const {name}: {};", self.ty(type_))
            }
            ItemEnum::Macro(def) => def.clone(),
            _ => String::new(),
        }
    }

    fn struct_sig(&self, name: &str, st: &Struct) -> String {
        let generics = self.render_generics(&st.generics);
        match &st.kind {
            StructKind::Unit => format!("struct {name}{generics};"),
            StructKind::Tuple(fields) => {
                let parts: Vec<String> = fields
                    .iter()
                    .map(|f| match f {
                        Some(id) => match &self.krate.index.get(id).map(|i| &i.inner) {
                            Some(ItemEnum::StructField(ty)) => self.ty(ty),
                            _ => "_".to_string(),
                        },
                        None => "/* private */".to_string(),
                    })
                    .collect();
                format!("struct {name}{generics}({});", parts.join(", "))
            }
            StructKind::Plain {
                fields,
                has_stripped_fields,
            } => {
                if fields.is_empty() {
                    return if *has_stripped_fields {
                        format!("struct {name}{generics} {{ /* private fields */ }}")
                    } else {
                        format!("struct {name}{generics};")
                    };
                }
                let mut s = format!("struct {name}{generics} {{\n");
                for id in fields {
                    if let Some(field) = self.krate.index.get(id) {
                        if let ItemEnum::StructField(ty) = &field.inner {
                            let fname = field.name.clone().unwrap_or_default();
                            let _ = writeln!(s, "    pub {fname}: {},", self.ty(ty));
                        }
                    }
                }
                if *has_stripped_fields {
                    s.push_str("    // some private fields omitted\n");
                }
                s.push('}');
                s
            }
        }
    }

    fn enum_sig(&self, name: &str, en: &Enum) -> String {
        let mut s = format!("enum {name}{} {{\n", self.render_generics(&en.generics));
        for id in &en.variants {
            if let Some(item) = self.krate.index.get(id) {
                if let ItemEnum::Variant(v) = &item.inner {
                    let vname = item.name.clone().unwrap_or_default();
                    let _ = writeln!(s, "    {},", self.variant(&vname, v));
                }
            }
        }
        s.push('}');
        s
    }

    fn variant(&self, name: &str, v: &Variant) -> String {
        match &v.kind {
            VariantKind::Plain => name.to_string(),
            VariantKind::Tuple(fields) => {
                let parts: Vec<String> = fields
                    .iter()
                    .map(|f| match f {
                        Some(id) => match self.krate.index.get(id).map(|i| &i.inner) {
                            Some(ItemEnum::StructField(ty)) => self.ty(ty),
                            _ => "_".to_string(),
                        },
                        None => "_".to_string(),
                    })
                    .collect();
                format!("{name}({})", parts.join(", "))
            }
            VariantKind::Struct { fields, .. } => {
                let parts: Vec<String> = fields
                    .iter()
                    .filter_map(|id| self.krate.index.get(id))
                    .filter_map(|field| match &field.inner {
                        ItemEnum::StructField(ty) => {
                            Some(format!("{}: {}", field.name.clone().unwrap_or_default(), self.ty(ty)))
                        }
                        _ => None,
                    })
                    .collect();
                format!("{name} {{ {} }}", parts.join(", "))
            }
        }
    }

    fn trait_sig(&self, name: &str, tr: &Trait) -> String {
        let mut s = String::new();
        if tr.is_unsafe {
            s.push_str("unsafe ");
        }
        let _ = write!(s, "trait {name}{}", self.render_generics(&tr.generics));
        if !tr.bounds.is_empty() {
            let bounds: Vec<String> = tr.bounds.iter().map(|b| self.bound(b)).collect();
            let _ = write!(s, ": {}", bounds.join(" + "));
        }
        s.push_str(&self.render_where(&tr.generics));
        s.push_str(" {\n");
        for id in &tr.items {
            if let Some(member) = self.krate.index.get(id) {
                let one = self.signature(member);
                for line in one.lines() {
                    let _ = writeln!(s, "    {line}");
                }
                // Methods/consts print their own signature; terminate the decl.
                if matches!(
                    member.inner,
                    ItemEnum::Function(_) | ItemEnum::Constant { .. } | ItemEnum::AssocType { .. }
                ) {
                    // trim trailing block we may have added; keep it simple:
                }
            }
        }
        s.push('}');
        s
    }

    /// A `### name` item's "Related" block: for a struct/enum, the traits it
    /// implements; for a trait, its implementors. Cross-linked where the type
    /// is one we generated a page for.
    fn related(&self, out: &mut String, item: &Item) {
        let mut lines: Vec<String> = Vec::new();
        match &item.inner {
            ItemEnum::Struct(st) => {
                for id in &st.impls {
                    if let Some(t) = self.impl_trait_name(id) {
                        lines.push(t);
                    }
                }
            }
            ItemEnum::Enum(en) => {
                for id in &en.impls {
                    if let Some(t) = self.impl_trait_name(id) {
                        lines.push(t);
                    }
                }
            }
            _ => {}
        }
        lines.sort();
        lines.dedup();
        if !lines.is_empty() {
            let _ = writeln!(out, "**Implements:** {}\n", lines.join(", "));
        }
    }

    fn impl_trait_name(&self, id: &Id) -> Option<String> {
        let item = self.krate.index.get(id)?;
        let ItemEnum::Impl(imp) = &item.inner else { return None };
        // Skip the impls rustdoc hides by default: compiler-synthesised auto
        // traits (Send/Sync/Unpin/...) and blanket impls (From/Into/TryFrom/...).
        // What is left is what the crate itself chose to implement.
        if imp.is_synthetic || imp.blanket_impl.is_some() {
            return None;
        }
        let path = imp.trait_.as_ref()?;
        Some(format!("`{}`", path.path))
    }

    // ----- generics / where / bounds -----

    fn render_generics(&self, g: &rustdoc_types::Generics) -> String {
        if g.params.is_empty() {
            return String::new();
        }
        let parts: Vec<String> = g
            .params
            .iter()
            .filter_map(|p| match &p.kind {
                rustdoc_types::GenericParamDefKind::Lifetime { .. } => Some(p.name.clone()),
                rustdoc_types::GenericParamDefKind::Type { .. } => Some(p.name.clone()),
                rustdoc_types::GenericParamDefKind::Const { type_, .. } => {
                    Some(format!("const {}: {}", p.name, self.ty(type_)))
                }
            })
            .collect();
        if parts.is_empty() {
            String::new()
        } else {
            format!("<{}>", parts.join(", "))
        }
    }

    fn render_where(&self, g: &rustdoc_types::Generics) -> String {
        if g.where_predicates.is_empty() {
            return String::new();
        }
        let parts: Vec<String> = g
            .where_predicates
            .iter()
            .filter_map(|w| match w {
                WherePredicate::BoundPredicate { type_, bounds, .. } => {
                    let bs: Vec<String> = bounds.iter().map(|b| self.bound(b)).collect();
                    if bs.is_empty() {
                        None
                    } else {
                        Some(format!("{}: {}", self.ty(type_), bs.join(" + ")))
                    }
                }
                _ => None,
            })
            .collect();
        if parts.is_empty() {
            String::new()
        } else {
            format!(" where {}", parts.join(", "))
        }
    }

    fn bound(&self, b: &GenericBound) -> String {
        match b {
            GenericBound::TraitBound { trait_, .. } => trait_.path.clone(),
            GenericBound::Outlives(lt) => lt.clone(),
            GenericBound::Use(_) => String::new(),
        }
    }

    // ----- Type pretty-printer -----

    fn render_self(&self, ty: &Type) -> String {
        // `self`, `&self`, `&mut self` read better than the spelled-out type.
        match ty {
            Type::BorrowedRef { is_mutable, .. } => {
                if *is_mutable {
                    "&mut self".to_string()
                } else {
                    "&self".to_string()
                }
            }
            _ => "self".to_string(),
        }
    }

    fn ty(&self, ty: &Type) -> String {
        match ty {
            Type::ResolvedPath(p) => {
                let mut s = p.path.clone();
                if let Some(args) = &p.args {
                    s.push_str(&self.generic_args(args));
                }
                s
            }
            Type::DynTrait(dt) => {
                let traits: Vec<String> =
                    dt.traits.iter().map(|t| t.trait_.path.clone()).collect();
                format!("dyn {}", traits.join(" + "))
            }
            Type::Generic(g) => g.clone(),
            // rustdoc spells the never type as the primitive "never"; write `!`.
            Type::Primitive(p) if p == "never" => "!".to_string(),
            Type::Primitive(p) => p.clone(),
            Type::FunctionPointer(_) => "fn(..)".to_string(),
            Type::Tuple(items) => {
                let parts: Vec<String> = items.iter().map(|t| self.ty(t)).collect();
                format!("({})", parts.join(", "))
            }
            Type::Slice(inner) => format!("[{}]", self.ty(inner)),
            Type::Array { type_, len } => format!("[{}; {}]", self.ty(type_), len),
            Type::ImplTrait(bounds) => {
                let bs: Vec<String> = bounds.iter().map(|b| self.bound(b)).collect();
                format!("impl {}", bs.join(" + "))
            }
            Type::Infer => "_".to_string(),
            Type::RawPointer { is_mutable, type_ } => {
                let m = if *is_mutable { "mut" } else { "const" };
                format!("*{m} {}", self.ty(type_))
            }
            Type::BorrowedRef {
                lifetime,
                is_mutable,
                type_,
            } => {
                let mut s = String::from("&");
                if let Some(lt) = lifetime {
                    let _ = write!(s, "{lt} ");
                }
                if *is_mutable {
                    s.push_str("mut ");
                }
                s.push_str(&self.ty(type_));
                s
            }
            Type::QualifiedPath {
                name, self_type, ..
            } => format!("{}::{}", self.ty(self_type), name),
            _ => "_".to_string(),
        }
    }

    fn generic_args(&self, args: &GenericArgs) -> String {
        match args {
            GenericArgs::AngleBracketed { args, constraints } => {
                let mut parts: Vec<String> = Vec::new();
                for a in args {
                    match a {
                        GenericArg::Lifetime(lt) => parts.push(lt.clone()),
                        GenericArg::Type(t) => parts.push(self.ty(t)),
                        GenericArg::Const(c) => parts.push(c.expr.clone()),
                        GenericArg::Infer => parts.push("_".to_string()),
                    }
                }
                for b in constraints {
                    parts.push(self.binding(b));
                }
                if parts.is_empty() {
                    String::new()
                } else {
                    format!("<{}>", parts.join(", "))
                }
            }
            GenericArgs::Parenthesized { inputs, output } => {
                let ins: Vec<String> = inputs.iter().map(|t| self.ty(t)).collect();
                let mut s = format!("({})", ins.join(", "));
                if let Some(o) = output {
                    let _ = write!(s, " -> {}", self.ty(o));
                }
                s
            }
            // `T::method(..)` associated-fn call notation -- rare in this API.
            _ => String::new(),
        }
    }

    fn binding(&self, b: &AssocItemConstraint) -> String {
        match &b.binding {
            AssocItemConstraintKind::Equality(term) => {
                let rhs = match term {
                    Term::Type(t) => self.ty(t),
                    Term::Constant(c) => c.expr.clone(),
                };
                format!("{} = {}", b.name, rhs)
            }
            AssocItemConstraintKind::Constraint(bounds) => {
                let bs: Vec<String> = bounds.iter().map(|x| self.bound(x)).collect();
                format!("{}: {}", b.name, bs.join(" + "))
            }
        }
    }
}

// ----- naming / slugs -----

/// Turn a module path (`["api", "bus", "spi"]`) into an output-relative slug
/// (`api/bus/spi`). The crate root becomes `<crate>/index`.
fn slug_for(path: &[String]) -> String {
    if path.len() == 1 {
        format!("{}/index", sanitize(&path[0]))
    } else {
        path.iter().map(|s| sanitize(s)).collect::<Vec<_>>().join("/")
    }
}

/// The site URL a page is reachable at (Starlight strips `/index` and lowercases).
fn url_for(path: &[String]) -> String {
    let slug = slug_for(path);
    let slug = slug.strip_suffix("/index").unwrap_or(&slug);
    format!("/api/{}/", slug.to_lowercase())
}

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .to_lowercase()
}

fn first_line(docs: Option<&str>) -> String {
    docs.and_then(|d| d.lines().find(|l| !l.trim().is_empty()))
        .unwrap_or("")
        .trim()
        .to_string()
}

fn yaml_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// The generated `API` sidebar group as JSON, for the Starlight config to splice.
fn build_sidebar(crates: &[CrateDoc]) -> String {
    // Shape: [{ "label": "<crate>", "items": [{ "label": "<mod path>", "link": "<url>" }] }]
    // Lead with the landing page so the group has an "Overview" front door.
    let mut roots: Vec<String> = vec![
        "  { \"label\": \"Overview\", \"link\": \"/api/\" }".to_string(),
    ];
    for c in crates {
        let mut entries: Vec<String> = Vec::new();
        for (path, _slug) in &c.modules {
            let label = path.join("::");
            let link = url_for(path);
            entries.push(format!(
                "    {{ \"label\": \"{}\", \"link\": \"{}\" }}",
                json_escape(&label),
                link
            ));
        }
        roots.push(format!(
            "  {{\n    \"label\": \"{}\",\n    \"collapsed\": true,\n    \"items\": [\n{}\n    ]\n  }}",
            json_escape(&c.name),
            entries.join(",\n")
        ));
    }
    format!("[\n{}\n]\n", roots.join(",\n"))
}

fn json_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

// Silence unused-import churn while the related-items model grows.
#[allow(dead_code)]
fn _touch(_: &HashMap<Id, Item>) {}
