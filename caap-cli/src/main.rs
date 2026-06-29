fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_target(false)
        .init();
    std::process::exit(caap_cli::commands::main_with_stdio(
        std::env::args().skip(1),
    ));
}
