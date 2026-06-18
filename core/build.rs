//! Compile proto/searcher.proto into a tonic gRPC client for Jito's
//! searcher.SearcherService (Day 5-6 leader-window timing, ADR 0008).

fn main() {
    // Reproducible protoc via a vendored binary — no system protoc needed, so the
    // docker build stays a plain `rust:slim` with no apt-get. Only set it when the
    // operator/CI hasn't deliberately exported a PROTOC of their own.
    if std::env::var_os("PROTOC").is_none() {
        std::env::set_var(
            "PROTOC",
            protoc_bin_vendored::protoc_bin_path().expect("vendored protoc binary"),
        );
    }

    tonic_prost_build::configure()
        .build_server(false) // client-only — we never serve this RPC
        .compile_protos(&["proto/searcher.proto"], &["proto"])
        .expect("compile proto/searcher.proto");

    println!("cargo:rerun-if-changed=proto/searcher.proto");
}
