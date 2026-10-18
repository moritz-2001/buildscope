//! Buildscope command-line entry point; implementation is shared with integration tests.
fn main() {
    match buildscope::cli::run() {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("buildscope: {error:#}");
            std::process::exit(1);
        }
    }
}
