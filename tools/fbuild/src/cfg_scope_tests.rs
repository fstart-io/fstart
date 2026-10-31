//! Audit real cfg syntax, not comments/strings, so new first-party consumers
//! cannot silently lose their selectors. Exported macros expand in the board
//! (which always receives all selectors), not in their defining library.
use proc_macro2::{Delimiter, TokenStream, TokenTree};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};
use syn::{Meta, Token, parse::Parser, punctuated::Punctuated, visit::Visit};

#[derive(Default)]
struct CfgUses(BTreeSet<String>);
fn selector(name: &str) -> bool {
    matches!(name, "fstart_entry" | "fstart_stage_env" | "fstart_payload")
}
impl CfgUses {
    fn condition(&mut self, tokens: TokenStream) {
        if let Ok(meta) = syn::parse2::<Meta>(tokens.clone()) {
            self.meta(&meta);
        } else {
            self.identifiers(tokens);
        } // macro metavariables in cfg syntax
    }
    fn identifiers(&mut self, tokens: TokenStream) {
        for token in tokens {
            match token {
                TokenTree::Ident(id) if selector(&id.to_string()) => {
                    self.0.insert(id.to_string());
                }
                TokenTree::Group(group) => self.identifiers(group.stream()),
                _ => {}
            }
        }
    }
    fn meta(&mut self, meta: &Meta) {
        let path = meta.path();
        if let Some(id) = path.get_ident().filter(|id| selector(&id.to_string())) {
            self.0.insert(id.to_string());
        }
        if let Meta::List(list) = meta {
            if let Ok(args) =
                Punctuated::<Meta, Token![,]>::parse_terminated.parse2(list.tokens.clone())
            {
                if path.is_ident("cfg_attr") {
                    let mut args = args.iter();
                    if let Some(condition) = args.next() {
                        self.meta(condition);
                    }
                    for attr in args.filter(|attr| {
                        attr.path().is_ident("cfg") || attr.path().is_ident("cfg_attr")
                    }) {
                        self.meta(attr);
                    }
                } else if ["cfg", "all", "any", "not"]
                    .iter()
                    .any(|name| path.is_ident(name))
                {
                    for arg in &args {
                        self.meta(arg);
                    }
                }
            } else if path.is_ident("cfg") || path.is_ident("cfg_attr") {
                self.identifiers(list.tokens.clone());
            }
        }
    }
    fn macro_attributes(&mut self, tokens: TokenStream) {
        let tokens: Vec<_> = tokens.into_iter().collect();
        for (index, token) in tokens.iter().enumerate() {
            if matches!(token, TokenTree::Punct(p) if p.as_char() == '#') {
                if let Some(TokenTree::Group(group)) = tokens.get(index+1).filter(|token| matches!(token, TokenTree::Group(group) if group.delimiter() == Delimiter::Bracket)) {
                    if let Ok(meta) = syn::parse2::<Meta>(group.stream()) {
                        if meta.path().is_ident("cfg") || meta.path().is_ident("cfg_attr") { self.meta(&meta); }
                    }
                }
            }
            if matches!(token, TokenTree::Ident(id) if id == "cfg")
                && matches!(tokens.get(index+1), Some(TokenTree::Punct(p)) if p.as_char() == '!')
            {
                if let Some(TokenTree::Group(group)) = tokens.get(index + 2) {
                    self.condition(group.stream());
                }
            }
            if let TokenTree::Group(group) = token {
                self.macro_attributes(group.stream());
            }
        }
    }
}
impl<'ast> Visit<'ast> for CfgUses {
    fn visit_attribute(&mut self, attr: &'ast syn::Attribute) {
        if attr.path().is_ident("cfg") || attr.path().is_ident("cfg_attr") {
            self.meta(&attr.meta);
        }
    }
    fn visit_item_macro(&mut self, item: &'ast syn::ItemMacro) {
        for attr in &item.attrs {
            self.visit_attribute(attr);
        }
        if !item
            .attrs
            .iter()
            .any(|attr| attr.path().is_ident("macro_export"))
        {
            self.visit_macro(&item.mac);
        }
    }
    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        if mac.path.is_ident("cfg") {
            self.condition(mac.tokens.clone());
        } else {
            self.macro_attributes(mac.tokens.clone());
        }
    }
}
fn sources(directory: &Path, files: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(directory).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_dir() {
            sources(&entry.path(), files);
        } else if entry
            .path()
            .extension()
            .is_some_and(|extension| extension == "rs")
        {
            files.push(entry.path());
        }
    }
}

#[test]
fn every_first_party_cfg_consumer_declares_its_selection_axes() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../crates");
    for entry in fs::read_dir(root).unwrap() {
        let package = entry.unwrap().path();
        if !package.join("src").is_dir() {
            continue;
        }
        let manifest: toml::Value =
            toml::from_str(&fs::read_to_string(package.join("Cargo.toml")).unwrap()).unwrap();
        let declared = manifest
            .get("package")
            .and_then(|v| v.get("metadata"))
            .and_then(|v| v.get("fstart"))
            .and_then(|v| v.get("compiler-cfg"))
            .and_then(toml::Value::as_table);
        let mut files = Vec::new();
        sources(&package.join("src"), &mut files);
        if package.join("build.rs").exists() {
            files.push(package.join("build.rs"));
        }
        for path in files {
            let file = syn::parse_file(&fs::read_to_string(&path).unwrap())
                .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
            let mut uses = CfgUses::default();
            uses.visit_file(&file);
            for axis in uses.0 {
                assert_ne!(
                    path.file_name().unwrap(),
                    "build.rs",
                    "{}: build scripts must not read target selection cfgs",
                    path.display()
                );
                assert!(
                    declared.is_some_and(|declared| declared.contains_key(&axis)),
                    "{} reads {axis} without package.metadata.fstart.compiler-cfg",
                    path.display()
                );
            }
        }
    }
}
#[test]
fn audit_distinguishes_cfg_positions_from_comments_strings_and_exported_macros() {
    let file = syn::parse_file(r##"
        // fstart_entry
        const TEXT: &str = "#[cfg(fstart_payload=\"linux\")]";
        #[macro_export] macro_rules! in_the_board { () => { #[cfg(fstart_entry="x86_64")] fn f() {} }; }
        #[cfg_attr(feature="x", cfg(fstart_stage_env="ram"))] fn local() { let _ = cfg!(fstart_payload="halt"); }
    "##).unwrap();
    let mut uses = CfgUses::default();
    uses.visit_file(&file);
    assert_eq!(
        uses.0,
        BTreeSet::from(["fstart_stage_env".into(), "fstart_payload".into()])
    );
}
