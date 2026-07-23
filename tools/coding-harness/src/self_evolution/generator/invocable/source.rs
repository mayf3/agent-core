use super::super::GenerationError;
use std::collections::BTreeSet;

pub(super) fn normalize(source: &str) -> Result<String, GenerationError> {
    if source.is_empty() || source.len() > 96 * 1024 || source.contains('\0') {
        return Err(invalid());
    }
    let mut syntax = syn::parse_file(source)
        .map_err(|_| GenerationError::new("GENERATOR_MODEL_OUTPUT_INVALID_RUST"))?;
    if syntax.items.iter().any(|item| match item {
        syn::Item::Use(item) => !allowed_use(&item.tree, &mut Vec::new()),
        syn::Item::ExternCrate(_) => true,
        _ => false,
    }) {
        return Err(unsafe_source());
    }
    syntax
        .items
        .retain(|item| !matches!(item, syn::Item::Use(_)));
    let public_functions: BTreeSet<String> = syntax
        .items
        .iter()
        .filter_map(|item| match item {
            syn::Item::Fn(function) if matches!(function.vis, syn::Visibility::Public(_)) => {
                Some(function.sig.ident.to_string())
            }
            _ => None,
        })
        .collect();
    if public_functions != BTreeSet::from(["transform".to_string()])
        || syntax.items.iter().any(public_non_function)
    {
        return Err(GenerationError::new(
            "GENERATOR_MODEL_OUTPUT_INTERFACE_MISMATCH",
        ));
    }
    let normalized = prettyplease::unparse(&syntax);
    for forbidden in [
        "unsafe",
        "extern crate",
        "extern \"",
        "std::",
        "core::",
        "alloc::",
        "crate::",
        "serde_json::",
        "include!",
        "include_str!",
        "include_bytes!",
        "env!",
        "option_env!",
        "macro_rules!",
        "#[path",
        "#[link",
        "asm!",
        "Command::",
        "File::",
        "TcpStream",
        "TcpListener",
        "UdpSocket",
        "UnixStream",
    ] {
        if normalized.contains(forbidden) {
            return Err(unsafe_source());
        }
    }
    let mut policy = SourcePolicy::default();
    syn::visit::Visit::visit_file(&mut policy, &syntax);
    if policy.denied {
        return Err(unsafe_source());
    }
    Ok(normalized)
}

fn public_non_function(item: &syn::Item) -> bool {
    let visibility = match item {
        syn::Item::Const(value) => Some(&value.vis),
        syn::Item::Enum(value) => Some(&value.vis),
        syn::Item::Mod(value) => Some(&value.vis),
        syn::Item::Static(value) => Some(&value.vis),
        syn::Item::Struct(value) => Some(&value.vis),
        syn::Item::Trait(value) => Some(&value.vis),
        syn::Item::Type(value) => Some(&value.vis),
        syn::Item::Union(value) => Some(&value.vis),
        _ => None,
    };
    visibility.is_some_and(|value| matches!(value, syn::Visibility::Public(_)))
}

#[derive(Default)]
struct SourcePolicy {
    denied: bool,
}

impl<'ast> syn::visit::Visit<'ast> for SourcePolicy {
    fn visit_path(&mut self, path: &'ast syn::Path) {
        if path.segments.first().is_some_and(|segment| {
            matches!(
                segment.ident.to_string().as_str(),
                "std" | "core" | "alloc" | "crate" | "super" | "serde" | "serde_json"
            )
        }) {
            self.denied = true;
            return;
        }
        syn::visit::visit_path(self, path);
    }

    fn visit_item_use(&mut self, _: &'ast syn::ItemUse) {
        self.denied = true;
    }

    fn visit_item_foreign_mod(&mut self, _: &'ast syn::ItemForeignMod) {
        self.denied = true;
    }

    fn visit_expr_unsafe(&mut self, _: &'ast syn::ExprUnsafe) {
        self.denied = true;
    }

    fn visit_macro(&mut self, value: &'ast syn::Macro) {
        let allowed = value.path.segments.last().is_some_and(|segment| {
            matches!(
                segment.ident.to_string().as_str(),
                "json" | "format" | "vec" | "matches"
            )
        });
        if !allowed {
            self.denied = true;
        }
    }
}

fn allowed_use(tree: &syn::UseTree, prefix: &mut Vec<String>) -> bool {
    match tree {
        syn::UseTree::Path(path) => {
            prefix.push(path.ident.to_string());
            let result = allowed_use(&path.tree, prefix);
            prefix.pop();
            result
        }
        syn::UseTree::Name(name) => {
            let mut path = prefix.clone();
            path.push(name.ident.to_string());
            path.len() == 2
                && path.first().map(String::as_str) == Some("serde_json")
                && matches!(path[1].as_str(), "json" | "Map" | "Value")
        }
        syn::UseTree::Group(group) => group.items.iter().all(|item| allowed_use(item, prefix)),
        syn::UseTree::Glob(_) | syn::UseTree::Rename(_) => false,
    }
}

fn invalid() -> GenerationError {
    GenerationError::new("GENERATOR_MODEL_OUTPUT_INVALID")
}

fn unsafe_source() -> GenerationError {
    GenerationError::new("GENERATOR_MODEL_OUTPUT_UNSAFE")
}
