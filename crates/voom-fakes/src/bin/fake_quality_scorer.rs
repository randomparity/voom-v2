#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<(), voom_worker_protocol::WorkerStartupError> {
    voom_fake_support::run_provider("fake-quality-scorer").await
}
