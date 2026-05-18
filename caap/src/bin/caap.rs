fn main() {
    std::process::exit(caap_core_port::cli::main_with_stdio(
        std::env::args().skip(1),
    ));
}
