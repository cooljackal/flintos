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

    // Parse every crate up front, because cross-linking is a two-phase job:
    // phase 1 builds a workspace-wide map from an item's fully-qualified path
    // (crate::mod::Name) to the site URL of the anchor it renders at, so phase 2
    // can turn a type mentioned in one crate into a link into another.
    let parsed: Vec<Crate> = json_files
        .iter()
        .map(|path| {
            let text = fs::read_to_string(path)
                .unwrap_or_else(|e| fatal(&format!("read {}: {e}", path.display())));
            serde_json::from_str(&text).unwrap_or_else(|e| {
                fatal(&format!(
                    "parse {} (format mismatch? this tool pins FORMAT_VERSION 57): {e}",
                    path.display()
                ))
            })
        })
        .collect();

    let mut links: Links = HashMap::new();
    for krate in &parsed {
        collect_links(krate, &mut links);
    }

    let mut crates: Vec<CrateDoc> = Vec::new();
    for krate in &parsed {
        crates.push(CrateDoc::render(krate, &links, &out_dir));
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
    fn render(krate: &Crate, links: &Links, out_dir: &Path) -> CrateDoc {
        let root = krate.index.get(&krate.root);
        let crate_name = root
            .and_then(|i| i.name.clone())
            .unwrap_or_else(|| "crate".to_string());
        let summary = first_line(root.and_then(|i| i.docs.as_deref()));

        let ctx = Ctx { krate, links };
        let mut modules = Vec::new();
        let mut pages = 0usize;

        // Walk the module tree depth-first from the crate root, emitting one
        // page per module. `path` accumulates the human module path
        // (crate::mod::sub) which is both the page title and its slug.
        let mut stack: Vec<(Id, Vec<String>)> = vec![(krate.root, vec![crate_name.clone()])];
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
                        stack.push((*child_id, cp));
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
        let url = url_for(std::slice::from_ref(&c.name));
        if c.summary.is_empty() {
            let _ = writeln!(s, "- [`{}`]({})", c.name, url);
        } else {
            let _ = writeln!(s, "- [`{}`]({}) — {}", c.name, url, c.summary);
        }
    }
    fs::write(out_dir.join("index.md"), s).unwrap_or_else(|e| fatal(&format!("write landing: {e}")));
}

/// A workspace-wide map: an item's fully-qualified path (`crate::mod::Name`) to
/// the site URL of the anchor it renders at. Built in phase 1, read in phase 2
/// to turn a type reference into a cross-crate link.
type Links = HashMap<String, String>;

/// Phase 1: record every module-level item's qualified path -> anchor URL. The
/// walk mirrors phase-2 rendering (same module tree, same anchor rule) so the
/// URLs line up. Nested items (methods, fields) are not separately linkable.
fn collect_links(krate: &Crate, links: &mut Links) {
    let Some(root) = krate.index.get(&krate.root) else { return };
    let crate_name = root.name.clone().unwrap_or_else(|| "crate".to_string());
    let mut stack: Vec<(Id, Vec<String>)> = vec![(krate.root, vec![crate_name])];
    while let Some((id, path)) = stack.pop() {
        let Some(item) = krate.index.get(&id) else { continue };
        let ItemEnum::Module(module) = &item.inner else { continue };
        let page = url_for(&path);
        for child_id in &module.items {
            let Some(child) = krate.index.get(child_id) else { continue };
            let Some(name) = &child.name else { continue };
            match &child.inner {
                ItemEnum::Module(_) => {
                    let mut cp = path.clone();
                    cp.push(name.clone());
                    stack.push((*child_id, cp));
                }
                ItemEnum::Struct(_)
                | ItemEnum::Enum(_)
                | ItemEnum::Trait(_)
                | ItemEnum::Function(_)
                | ItemEnum::TypeAlias(_)
                | ItemEnum::Constant { .. }
                | ItemEnum::Macro(_) => {
                    let qual = format!("{}::{}", path.join("::"), name);
                    let url = format!("{page}#{}", anchor(name));
                    links.insert(qual, url);
                }
                _ => {}
            }
        }
    }
}

/// The heading anchor Starlight (github-slugger) gives `### name`. Item names are
/// identifiers (alphanumerics + `_`), so this is just a lowercase.
fn anchor(name: &str) -> String {
    name.to_lowercase()
}

/// Collect the `(path, id)` of every nominal type (`ResolvedPath`) reachable
/// inside `ty`: through references, pointers, slices, arrays, tuples, and
/// generic arguments. Used to build an item's "References" links.
fn walk_paths(ty: &Type, out: &mut Vec<(String, Id)>) {
    match ty {
        Type::ResolvedPath(p) => {
            out.push((p.path.clone(), p.id));
            if let Some(args) = &p.args {
                walk_args(args, out);
            }
        }
        Type::BorrowedRef { type_, .. } | Type::RawPointer { type_, .. } => walk_paths(type_, out),
        Type::Slice(inner) => walk_paths(inner, out),
        Type::Array { type_, .. } => walk_paths(type_, out),
        Type::Tuple(items) => items.iter().for_each(|t| walk_paths(t, out)),
        Type::QualifiedPath { self_type, .. } => walk_paths(self_type, out),
        _ => {}
    }
}

fn walk_args(args: &GenericArgs, out: &mut Vec<(String, Id)>) {
    match args {
        GenericArgs::AngleBracketed { args, .. } => {
            for a in args {
                if let GenericArg::Type(t) = a {
                    walk_paths(t, out);
                }
            }
        }
        GenericArgs::Parenthesized { inputs, output } => {
            inputs.iter().for_each(|t| walk_paths(t, out));
            if let Some(o) = output {
                walk_paths(o, out);
            }
        }
        _ => {}
    }
}

/// Shared read-only view over one crate while rendering it, plus the workspace
/// link map for cross-references.
struct Ctx<'a> {
    krate: &'a Crate,
    links: &'a Links,
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

    /// A `### name` item's cross-reference block, rendered *outside* the code
    /// fence (where markdown links would be inert): the traits a struct/enum
    /// implements, and the named types its signature mentions -- each a link
    /// wherever the target is an item we generated a page for, in this crate or
    /// any other.
    fn related(&self, out: &mut String, item: &Item) {
        // Implements: the crate's own trait impls (auto/blanket ones filtered).
        let impls = match &item.inner {
            ItemEnum::Struct(st) => &st.impls[..],
            ItemEnum::Enum(en) => &en.impls[..],
            _ => &[][..],
        };
        let mut implemented: Vec<String> = impls
            .iter()
            .filter_map(|id| self.impl_trait_link(id))
            .collect();
        implemented.sort();
        implemented.dedup();
        if !implemented.is_empty() {
            let _ = writeln!(out, "**Implements:** {}\n", implemented.join(", "));
        }

        // References: named types mentioned in the signature that we can link.
        let self_name = item.name.clone().unwrap_or_default();
        let mut refs: Vec<(String, Id)> = Vec::new();
        self.referenced_paths(item, &mut refs);
        let mut links: Vec<String> = refs
            .iter()
            .filter(|(name, _)| *name != self_name)
            .filter_map(|(name, id)| {
                let url = self.path_url(id)?;
                let short = name.rsplit("::").next().unwrap_or(name);
                Some(format!("[`{short}`]({url})"))
            })
            .collect();
        links.sort();
        links.dedup();
        if !links.is_empty() {
            let _ = writeln!(out, "**References:** {}\n", links.join(", "));
        }
    }

    /// A linked (or, if we did not generate its page, plain) code span for the
    /// trait an impl implements. `None` for the impls rustdoc hides by default:
    /// synthesised auto traits (Send/Sync/...) and blanket impls (From/Into/...).
    fn impl_trait_link(&self, id: &Id) -> Option<String> {
        let item = self.krate.index.get(id)?;
        let ItemEnum::Impl(imp) = &item.inner else { return None };
        if imp.is_synthetic || imp.blanket_impl.is_some() {
            return None;
        }
        let path = imp.trait_.as_ref()?;
        let short = path.path.rsplit("::").next().unwrap_or(&path.path);
        Some(match self.path_url(&path.id) {
            Some(url) => format!("[`{short}`]({url})"),
            None => format!("`{short}`"),
        })
    }

    /// The site URL for the item `id` names, via its fully-qualified path in the
    /// crate's `paths` table (which covers external items too), or `None` if it
    /// is not something we generated a page for (a primitive, a std type, ...).
    fn path_url(&self, id: &Id) -> Option<String> {
        let summary = self.krate.paths.get(id)?;
        self.links.get(&summary.path.join("::")).cloned()
    }

    /// Collect the `(name, id)` of every named type mentioned in `item`'s
    /// signature -- parameters, return, fields, variants, aliased type.
    fn referenced_paths(&self, item: &Item, out: &mut Vec<(String, Id)>) {
        match &item.inner {
            ItemEnum::Function(f) => {
                for (_, ty) in &f.sig.inputs {
                    walk_paths(ty, out);
                }
                if let Some(o) = &f.sig.output {
                    walk_paths(o, out);
                }
            }
            ItemEnum::Struct(st) => {
                let field_ids: Vec<Id> = match &st.kind {
                    StructKind::Plain { fields, .. } => fields.clone(),
                    StructKind::Tuple(fields) => fields.iter().flatten().cloned().collect(),
                    StructKind::Unit => Vec::new(),
                };
                for fid in field_ids {
                    if let Some(ItemEnum::StructField(ty)) =
                        self.krate.index.get(&fid).map(|i| &i.inner)
                    {
                        walk_paths(ty, out);
                    }
                }
            }
            ItemEnum::Enum(en) => {
                for vid in &en.variants {
                    if let Some(ItemEnum::Variant(v)) = self.krate.index.get(vid).map(|i| &i.inner) {
                        let fids: Vec<Id> = match &v.kind {
                            VariantKind::Tuple(fields) => fields.iter().flatten().cloned().collect(),
                            VariantKind::Struct { fields, .. } => fields.clone(),
                            VariantKind::Plain => Vec::new(),
                        };
                        for fid in fids {
                            if let Some(ItemEnum::StructField(ty)) =
                                self.krate.index.get(&fid).map(|i| &i.inner)
                            {
                                walk_paths(ty, out);
                            }
                        }
                    }
                }
            }
            ItemEnum::TypeAlias(a) => walk_paths(&a.type_, out),
            ItemEnum::Constant { type_, .. } => walk_paths(type_, out),
            _ => {}
        }
    }

    // ----- generics / where / bounds -----

    fn render_generics(&self, g: &rustdoc_types::Generics) -> String {
        if g.params.is_empty() {
            return String::new();
        }
        let parts: Vec<String> = g
            .params
            .iter()
            .map(|p| match &p.kind {
                rustdoc_types::GenericParamDefKind::Lifetime { .. } => p.name.clone(),
                rustdoc_types::GenericParamDefKind::Type { .. } => p.name.clone(),
                rustdoc_types::GenericParamDefKind::Const { type_, .. } => {
                    format!("const {}: {}", p.name, self.ty(type_))
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
/// Group a crate under a sidebar category by its name -- so 41 flat crates read
/// as a handful of sections (the esp32 drivers together, the buses together,
/// ...). Returns `(order, label)`; `order` fixes the section sequence.
fn category(name: &str) -> (u8, &'static str) {
    let n = name.replace('-', "_");
    match n.as_str() {
        "api" | "hal" | "kernel" | "board" => (0, "System"),
        _ if n.starts_with("arch_") => (1, "Architectures"),
        _ if n.starts_with("soc_") => (2, "SoCs"),
        _ if n.starts_with("esp32_") => (3, "Drivers · ESP32"),
        _ if n.ends_with("_bus") => (4, "Buses"),
        _ if n.starts_with("radio") => (5, "Radio"),
        _ => (6, "Drivers & libraries"),
    }
}

/// The generated API sidebar: an Overview link, then one section per category,
/// each a collapsed group whose items are the crates (themselves collapsed
/// groups of their module pages). Built with serde_json so the nesting is
/// correct by construction.
fn build_sidebar(crates: &[CrateDoc]) -> String {
    use serde_json::{json, Value};

    // Bucket crates by category, preserving a stable order within each.
    let mut ordered: Vec<&CrateDoc> = crates.iter().collect();
    ordered.sort_by(|a, b| a.name.cmp(&b.name));

    let mut items: Vec<Value> = vec![json!({ "label": "Overview", "link": "/api/" })];
    // Walk categories in fixed order; within a category, the crates in name order.
    let mut cats: Vec<(u8, &'static str)> = ordered.iter().map(|c| category(&c.name)).collect();
    cats.sort();
    cats.dedup();
    for (ord, label) in cats {
        let crate_groups: Vec<Value> = ordered
            .iter()
            .filter(|c| category(&c.name) == (ord, label))
            .map(|c| {
                let mods: Vec<Value> = c
                    .modules
                    .iter()
                    .map(|(path, _slug)| json!({ "label": path.join("::"), "link": url_for(path) }))
                    .collect();
                json!({ "label": c.name, "collapsed": true, "items": mods })
            })
            .collect();
        items.push(json!({ "label": label, "collapsed": true, "items": crate_groups }));
    }

    serde_json::to_string_pretty(&Value::Array(items)).unwrap_or_else(|_| "[]".to_string())
}
