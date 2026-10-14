//! symhow binary entry point: mirrors src/main.cpp -- a thin wrapper that
//! hands argv to the CLI layer and exits with its exit code.

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let code = symhow::cli::run(&args);
    std::process::exit(code);
}
