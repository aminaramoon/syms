//! symwhere — fast, fuzzy, incremental symbol index for C/C++/CUDA/Rust.

mod cli;
mod fuzzy;
mod index;
mod kinds;
mod lang;
mod output;
mod single;
mod store;
mod types;
mod update;
mod walk;

use crate::cli::{Cli, Command};
use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;

fn main() {
    let cli = Cli::parse();
    if let Err(err) = run(cli) {
        eprintln!("error: {err:#}");
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Index {
            path,
            lang,
            exclude,
            include,
            force,
        } => index::run_index(index::IndexOptions {
            path: path.unwrap_or_else(|| PathBuf::from(".")),
            langs: kinds::select_langs(&lang),
            excludes: exclude,
            includes: include,
            force,
            db: cli.db,
            jobs: cli.jobs,
        }),

        Command::Update {
            path,
            git_diff,
            dry_run,
            lang,
            exclude,
            include,
        } => update::run_update(update::UpdateOptions {
            path: path.unwrap_or_else(|| PathBuf::from(".")),
            git_diff,
            dry_run,
            langs: kinds::select_langs(&lang),
            excludes: exclude,
            includes: include,
            db: cli.db,
            jobs: cli.jobs,
        }),

        Command::Find {
            queries,
            r#type,
            lang,
            exclude,
            include,
            exact,
            limit,
            json,
            spans,
        } => fuzzy::run_find(fuzzy::FindOptions {
            queries,
            kinds: kinds::select_kinds(&r#type),
            langs: kinds::select_langs(&lang),
            excludes: exclude,
            includes: include,
            exact,
            limit,
            db: cli.db,
        })
        .map(|hits| output::emit(&hits, json, spans)),

        Command::Symbols {
            file,
            r#type,
            within,
            lang,
            json,
            spans,
        } => single::run_symbols(single::SymbolsOptions {
            file,
            kinds: kinds::select_kinds(&r#type),
            within,
            lang: lang.map(|l| l.into()),
        })
        .map(|syms| output::emit_symbols(&syms, json, spans)),

        Command::Langs => {
            output::print_langs();
            Ok(())
        }
    }
}
