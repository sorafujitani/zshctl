#[tokio::main]
async fn main() {
    let result = match zshctl_daemon::RuntimePaths::resolve() {
        Ok(paths) => zshctl_daemon::run(paths).await,
        Err(error) => Err(error),
    };
    if let Err(error) = result {
        eprintln!("zshctld: {error}");
        std::process::exit(1);
    }
}
