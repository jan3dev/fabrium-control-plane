//! The logs hold no token, no tenant key, and no payload.
//!
//! This test has a binary of its own. A log subscriber that is set for one thread misses
//! the events of call sites that another test of the same process hit first.

mod common;

use std::{
    io,
    sync::{Arc, Mutex},
    time::Duration,
};

use common::*;
use serde_json::json;
use tracing_subscriber::fmt::MakeWriter;

/// A log writer that keeps everything in memory.
#[derive(Clone, Default)]
struct LogBuf(Arc<Mutex<Vec<u8>>>);

impl io::Write for LogBuf {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl MakeWriter<'_> for LogBuf {
    type Writer = LogBuf;

    fn make_writer(&self) -> LogBuf {
        self.clone()
    }
}

#[tokio::test]
async fn logs_never_hold_a_token_a_key_or_the_payload() {
    let buf = LogBuf::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(buf.clone())
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .finish();
    // The test runs on one thread, so this default sees every log line of the handlers.
    let _guard = tracing::subscriber::set_default(subscriber);

    let h = harness().await;
    let mut body = create_body("lima");
    body["bootstrap"]["files"][0]["content"] = json!("MARKER-FILE-CONTENT");
    body["bootstrap"]["script"] = json!("echo MARKER-SCRIPT");
    let (_, vm) = create(&h.app, STAGE, &body).await;
    let token = token_in(&h.fake.launches()[0].user_data);
    // The error paths log too: a failed tag, a failed create, a failed delete.
    h.fake.fail_tagging(true);
    redeem(&h, &token).await;
    settle().await; // the tag call runs after the answer
    // A repeat from another address is refused, and logged with that address.
    redeem_from(&h, &token, "198.51.100.7").await;
    h.fake.fail(true);
    create(&h.app, STAGE, &create_body("other")).await;
    call(&h.app, "GET", "/v1/vms", Some(STAGE)).await;
    h.fake.fail(false);
    // A valid key from an address that its tenant does not use, or from none, is refused and logged.
    call_from(&h.app, "GET", "/v1/vms", Some(STAGE), Some(PROD_SOURCE)).await;
    call_from(&h.app, "GET", "/v1/vms", Some(STAGE), None).await;
    let uri = format!("/v1/vms/{}", vm["id"].as_str().unwrap());
    call_raw(&h.app, "DELETE", &uri, Some(STAGE)).await;
    // A tag call that hangs is cut off after 5 s, and that is logged too.
    h.fake.fail_tagging(false);
    create(&h.app, STAGE, &create_body("slow")).await;
    let slow_token = token_in(&h.fake.launches()[1].user_data);
    h.fake.hang_tagging(true);
    tokio::time::pause();
    redeem(&h, &slow_token).await;
    settle().await; // the tag call starts, and hangs
    tokio::time::advance(Duration::from_secs(6)).await;
    settle().await; // the 5 s limit fires

    let logs = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
    // The capture works: these lines exist.
    let lines = [
        "vm launched",
        "tagging",
        "aws request failed",
        "vm deleted",
        "timed out",
        "refused a repeat redeem from another address",
        "198.51.100.7",
    ];
    for line in lines {
        assert!(logs.contains(line), "no {line:?} in the log:\n{logs}");
    }
    // A refused source is logged with its tenant and its address, or `none`.
    let refused: Vec<&str> = (logs.lines())
        .filter(|l| l.contains("not a source of the tenant"))
        .collect();
    for from in [PROD_SOURCE, "none"] {
        let want = format!("tenant=stage from={from}");
        let found = refused.iter().any(|l| l.contains(&want));
        assert!(found, "no refused line with {want:?} in the log:\n{logs}");
    }
    for secret in [
        token.as_str(),
        slow_token.as_str(),
        "MARKER-FILE-CONTENT",
        "MARKER-SCRIPT",
        "stage-secret",
    ] {
        assert!(!logs.contains(secret), "{secret:?} is in the log:\n{logs}");
    }
}
