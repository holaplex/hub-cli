fn main() {
    prost_build::compile_protos(
        &[
            "src/proto/asset-upload.proto",
            "src/proto/drop-mint.proto",
            "src/proto/mint-random-queued.proto",
        ],
        &["src/proto"],
    )
    .unwrap();
}
