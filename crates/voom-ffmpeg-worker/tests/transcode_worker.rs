#[test]
fn binary_exists_for_protocol_worker_registration() {
    let binary = env!("CARGO_BIN_EXE_voom-ffmpeg-worker");
    assert!(std::path::Path::new(binary).exists());
}
