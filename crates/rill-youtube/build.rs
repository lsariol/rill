//! Generates the gRPC client for YouTube's live chat from the vendored proto, without `protoc`.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let proto = "proto/stream_list.proto";
    println!("cargo:rerun-if-changed={proto}");
    let files = protox::compile([proto], ["proto"])?;
    tonic_prost_build::configure()
        .build_server(true)
        .server_mod_attribute("youtube.api.v3", "#[cfg(test)]")
        .compile_fds(files)?;
    Ok(())
}
