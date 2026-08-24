fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 生成的代码写入 OUT_DIR，由 src/lib.rs 通过 include! 引入。
    tonic_build::configure()
        .build_client(true)
        .build_server(true)
        .server_mod_attribute(".", "#[allow(clippy::result_large_err)]")
        .client_mod_attribute(".", "#[allow(clippy::result_large_err)]")
        .compile_protos(&["proto/worker.proto"], &["proto"])?;
    println!("cargo:rerun-if-changed=proto/worker.proto");
    Ok(())
}
