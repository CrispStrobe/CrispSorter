fn main() {
    std::env::set_var(
        "PROTOC",
        protoc_bin_vendored::protoc_bin_path().expect("bundled protoc"),
    );
}
