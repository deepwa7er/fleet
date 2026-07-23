//! Structure out of rust-analyzer's SCIP symbol strings.
//!
//! A symbol like
//! `rust-analyzer cargo breakwater 0.1.0 tls/impl#[CertResolver][Debug]fmt().`
//! carries everything atlas needs to place it: crate, module path, the impl's
//! self type, the implemented trait, and the name. The descriptor grammar is
//! parsed by the scip crate; this module interprets rust-analyzer's use of it:
//!
//! - `Namespace` descriptors are the module path.
//! - A `Type` descriptor named `impl` followed by bracketed `TypeParameter`
//!   descriptors is an impl block: the first parameter is the self type, the
//!   second (when present) the implemented trait.
//! - Any other `Type` descriptor before the last is a containing type
//!   (enum members, fields, trait method declarations).
//! - The last descriptor is the symbol's own name.

use scip::types::descriptor::Suffix;
use scip::types::symbol_information::Kind;

/// What the trailing descriptor says the symbol is — the classification used
/// for external symbols, which have no `SymbolInformation.kind`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SuffixKind {
    Module,
    Type,
    Term,
    Function,
    Macro,
    Other,
}

#[derive(Debug, Clone)]
pub struct ParsedSymbol {
    pub crate_name: String,
    /// `a::b` for nested modules; empty at the crate root.
    pub module_path: String,
    pub name: String,
    /// The containing type: an impl's self type, a field's struct, an enum
    /// member's enum.
    pub container: Option<String>,
    /// For members of `impl Trait for Type`: the trait.
    pub trait_name: Option<String>,
    pub suffix_kind: SuffixKind,
}

/// Parse a global SCIP symbol. Returns `None` for locals and for strings the
/// SCIP grammar rejects (neither occurs for rust-analyzer's global symbols in
/// practice; callers treat `None` as "skip").
pub fn parse(symbol: &str) -> Option<ParsedSymbol> {
    if scip::symbol::is_local_symbol(symbol) {
        return None;
    }
    let parsed = scip::symbol::parse_symbol(symbol).ok()?;
    let crate_name = parsed.package.name.clone();
    let descriptors = parsed.descriptors;
    let last = descriptors.last()?;

    let mut modules: Vec<&str> = Vec::new();
    let mut container = None;
    let mut trait_name = None;

    for (i, d) in descriptors.iter().enumerate() {
        let is_last = i == descriptors.len() - 1;
        match d.suffix.enum_value_or_default() {
            Suffix::Namespace if !is_last => modules.push(&d.name),
            // `impl#` opens an impl block; its bracketed parameters follow.
            Suffix::Type if !is_last && d.name == "impl" => {}
            Suffix::Type if !is_last => container = Some(d.name.clone()),
            Suffix::TypeParameter if !is_last => match container {
                None => container = Some(d.name.clone()),
                Some(_) if trait_name.is_none() => trait_name = Some(d.name.clone()),
                Some(_) => {}
            },
            _ => {}
        }
    }

    let suffix_kind = match last.suffix.enum_value_or_default() {
        Suffix::Namespace => SuffixKind::Module,
        Suffix::Type => SuffixKind::Type,
        Suffix::Term => SuffixKind::Term,
        Suffix::Method => SuffixKind::Function,
        Suffix::Macro => SuffixKind::Macro,
        // The impl block itself ends in a bracketed type parameter
        // (e.g. `version/impl#[BuildInfo]`); its name is that type.
        Suffix::TypeParameter => SuffixKind::Type,
        _ => SuffixKind::Other,
    };

    Some(ParsedSymbol {
        crate_name,
        module_path: modules.join("::"),
        name: last.name.clone(),
        container,
        trait_name,
        suffix_kind,
    })
}

impl ParsedSymbol {
    /// Human path: `crate::module::Container::name`, with `as Trait` for
    /// trait-impl members — the label the UI shows for any symbol.
    pub fn display_path(&self) -> String {
        let mut out = self.crate_name.clone();
        if !self.module_path.is_empty() {
            out.push_str("::");
            out.push_str(&self.module_path);
        }
        match (&self.container, &self.trait_name) {
            (Some(c), Some(t)) => {
                out.push_str(&format!("::<{c} as {t}>"));
            }
            (Some(c), None) => {
                out.push_str("::");
                out.push_str(c);
            }
            (None, _) => {}
        }
        // The crate-root module's own descriptor is named `crate`; the crate
        // name already says that.
        if !(self.suffix_kind == SuffixKind::Module && self.name == "crate") {
            out.push_str("::");
            out.push_str(&self.name);
        }
        out
    }
}

/// Map a SCIP `SymbolInformation.kind` to atlas's kind string. `parsed` breaks
/// the tie for `UnspecifiedKind`, which rust-analyzer never emits today but
/// the format allows.
pub fn kind_str(kind: Kind, parsed: &ParsedSymbol) -> &'static str {
    match kind {
        Kind::Module => "module",
        Kind::Struct => "struct",
        Kind::Enum => "enum",
        Kind::EnumMember => "enum_member",
        Kind::Trait => "trait",
        Kind::TypeAlias => "type_alias",
        Kind::AssociatedType => "assoc_type",
        Kind::Function => "function",
        Kind::Method => "method",
        Kind::StaticMethod => "static_method",
        Kind::TraitMethod => "trait_method",
        Kind::Field => "field",
        Kind::Constant => "constant",
        Kind::StaticVariable => "static",
        Kind::Macro => "macro",
        _ => suffix_kind_str(parsed.suffix_kind),
    }
}

/// Kind string for symbols known only by their string (externals).
pub fn suffix_kind_str(suffix: SuffixKind) -> &'static str {
    match suffix {
        SuffixKind::Module => "module",
        SuffixKind::Type => "type",
        SuffixKind::Term => "value",
        SuffixKind::Function => "function",
        SuffixKind::Macro => "macro",
        SuffixKind::Other => "unknown",
    }
}

/// Whether a reference to this kind of symbol is a call (vs a use of a type
/// or value). Macros count: an invocation transfers to generated code.
pub fn kind_is_callable(kind: &str) -> bool {
    matches!(
        kind,
        "function" | "method" | "static_method" | "trait_method" | "macro"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_function_at_crate_root() {
        let p = parse("rust-analyzer cargo tugboat 0.1.0 main().").unwrap();
        assert_eq!(p.crate_name, "tugboat");
        assert_eq!(p.module_path, "");
        assert_eq!(p.name, "main");
        assert_eq!(p.container, None);
        assert_eq!(p.suffix_kind, SuffixKind::Function);
        assert_eq!(p.display_path(), "tugboat::main");
    }

    #[test]
    fn function_in_nested_module() {
        let p = parse("rust-analyzer cargo drydock 0.1.0 core/store/lock().").unwrap();
        assert_eq!(p.module_path, "core::store");
        assert_eq!(p.name, "lock");
        assert_eq!(p.display_path(), "drydock::core::store::lock");
    }

    #[test]
    fn inherent_impl_method() {
        let p = parse("rust-analyzer cargo breakwater 0.1.0 config/impl#[Config]load().").unwrap();
        assert_eq!(p.module_path, "config");
        assert_eq!(p.container.as_deref(), Some("Config"));
        assert_eq!(p.trait_name, None);
        assert_eq!(p.name, "load");
        assert_eq!(p.display_path(), "breakwater::config::Config::load");
    }

    #[test]
    fn trait_impl_method_carries_both_type_and_trait() {
        let p = parse("rust-analyzer cargo breakwater 0.1.0 tls/impl#[CertResolver][Debug]fmt().")
            .unwrap();
        assert_eq!(p.container.as_deref(), Some("CertResolver"));
        assert_eq!(p.trait_name.as_deref(), Some("Debug"));
        assert_eq!(p.name, "fmt");
        assert_eq!(
            p.display_path(),
            "breakwater::tls::<CertResolver as Debug>::fmt"
        );
    }

    #[test]
    fn enum_member_and_field_have_type_containers() {
        let member = parse("rust-analyzer cargo tugboat 0.1.0 Command#Deploy#").unwrap();
        assert_eq!(member.container.as_deref(), Some("Command"));
        assert_eq!(member.name, "Deploy");
        assert_eq!(member.suffix_kind, SuffixKind::Type);

        let field = parse("rust-analyzer cargo tugboat 0.1.0 Cli#command.").unwrap();
        assert_eq!(field.container.as_deref(), Some("Cli"));
        assert_eq!(field.name, "command");
        assert_eq!(field.suffix_kind, SuffixKind::Term);
    }

    #[test]
    fn trait_method_declaration() {
        let p = parse("rust-analyzer cargo tugboat 0.1.0 deploy/LogSink#line().").unwrap();
        assert_eq!(p.container.as_deref(), Some("LogSink"));
        assert_eq!(p.trait_name, None);
        assert_eq!(p.name, "line");
    }

    #[test]
    fn crate_root_module() {
        let p = parse("rust-analyzer cargo breakwater 0.1.0 crate/").unwrap();
        assert_eq!(p.suffix_kind, SuffixKind::Module);
        assert_eq!(p.name, "crate");
        assert_eq!(p.module_path, "");
        assert_eq!(p.display_path(), "breakwater");
    }

    #[test]
    fn nested_module_symbol() {
        let p = parse("rust-analyzer cargo drydock 0.1.0 core/store/").unwrap();
        assert_eq!(p.suffix_kind, SuffixKind::Module);
        assert_eq!(p.name, "store");
        assert_eq!(p.module_path, "core");
    }

    #[test]
    fn macro_symbol() {
        let p = parse("rust-analyzer cargo drydock 0.1.0 core/state/transition!").unwrap();
        assert_eq!(p.suffix_kind, SuffixKind::Macro);
        assert_eq!(p.name, "transition");
        assert_eq!(p.module_path, "core::state");
    }

    #[test]
    fn external_dependency_symbol() {
        let p = parse("rust-analyzer cargo tokio 1.0.0 net/tcp/listener/impl#[TcpListener]bind().")
            .unwrap();
        assert_eq!(p.crate_name, "tokio");
        assert_eq!(p.module_path, "net::tcp::listener");
        assert_eq!(p.container.as_deref(), Some("TcpListener"));
        assert_eq!(p.name, "bind");
    }

    #[test]
    fn locals_are_skipped() {
        assert!(parse("local 12").is_none());
    }
}
