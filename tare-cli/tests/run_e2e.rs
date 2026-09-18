//! `tare run` end-to-end: the dedicated proxy injects loopback env into the child,
//! the child (`tare __emit`) makes two loopback POSTs, and both steps land in the
//! store grouped under the run id. Fake upstream; loopback only.

use std::sync::Arc;
use tare_proxy::fake::FakeHandle;

async fn spawn_fake() -> (FakeHandle, String) {
    let fake = FakeHandle::new();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = fake.router();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (fake, format!("http://{addr}"))
}

#[test]
fn tare_run_injects_env_and_groups_steps() {
    std::env::set_var("TARE_NETWORK_GUARD", "loopback");

    // Stand up a fake Anthropic upstream with two canned responses.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let (fake, fake_base) = rt.block_on(spawn_fake());
    let resp = std::fs::read(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("fixtures/anthropic_nonstream/response.json"),
    )
    .unwrap();
    fake.enqueue_json(resp.clone());
    fake.enqueue_json(resp);

    // Point `tare run` upstreams at the fake; have __emit fire two requests.
    std::env::set_var("TARE_ANTHROPIC_UPSTREAM", &fake_base);
    std::env::set_var("TARE_OPENAI_UPSTREAM", &fake_base);
    std::env::set_var("TARE_EMIT_N", "2");

    let tmp = std::env::temp_dir().join(format!("tare-run-e2e-{}.db", std::process::id()));
    let db = tmp.to_string_lossy().to_string();
    let _ = std::fs::remove_file(&db);

    let tare_bin = env!("CARGO_BIN_EXE_tare").to_string();
    let child = vec![tare_bin, "__emit".to_string()];

    // Keep the fake handle alive for the duration of the run.
    let _keep = Arc::new(fake);
    let code = tare_cli::run_command(&child, &db, "run-e2e").expect("run_command");
    assert_eq!(code, 0);

    // Both captured steps are grouped under the forced run id.
    let store = tare_store::Store::open(&db).unwrap();
    let runs = store.load_runs().unwrap();
    let run = runs
        .iter()
        .find(|r| r.run_id == "run-e2e")
        .expect("run recorded");
    assert_eq!(run.steps.len(), 2);
    assert_eq!(run.steps[0].step_ordinal, 1);
    assert_eq!(run.steps[1].step_ordinal, 2);
    assert_eq!(run.steps[0].usage.output, 53); // from the canned (real) response

    let _ = std::fs::remove_file(&db);
}
