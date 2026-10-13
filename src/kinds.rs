//! Small conversion helpers between CLI args and core types.

use crate::cli::{KindArg, LangArg};
use crate::types::{Lang, SymbolKind};
use std::collections::BTreeSet;
use std::str::FromStr;

pub fn select_langs(args: &[LangArg]) -> BTreeSet<Lang> {
    if args.is_empty() {
        Lang::ALL.into_iter().collect()
    } else {
        args.iter()
            .map(|a| match a {
                LangArg::C => Lang::C,
                LangArg::Cpp => Lang::Cpp,
                LangArg::Cuda => Lang::Cuda,
                LangArg::Rust => Lang::Rust,
            })
            .collect()
    }
}

pub fn select_kinds(args: &[KindArg]) -> BTreeSet<SymbolKind> {
    if args.is_empty() {
        SymbolKind::ALL.into_iter().collect()
    } else {
        args.iter()
            .map(|a| {
                SymbolKind::from_str(match a {
                    KindArg::Class => "class",
                    KindArg::Struct => "struct",
                    KindArg::Enum => "enum",
                    KindArg::Union => "union",
                    KindArg::Interface => "interface",
                    KindArg::Trait => "trait",
                    KindArg::Impl => "impl",
                    KindArg::Function => "function",
                    KindArg::Method => "method",
                    KindArg::Constructor => "constructor",
                    KindArg::Field => "field",
                    KindArg::Constant => "constant",
                    KindArg::Static => "static",
                    KindArg::Variable => "variable",
                    KindArg::TypeAlias => "type-alias",
                    KindArg::Namespace => "namespace",
                    KindArg::Module => "module",
                    KindArg::Macro => "macro",
                    KindArg::EnumMember => "enum-member",
                    KindArg::Operator => "operator",
                })
                .expect("KindArg name maps to a valid SymbolKind")
            })
            .collect()
    }
}
