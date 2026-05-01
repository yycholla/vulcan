//! Wire protocol unit tests (Task 0.2 of YYC-266 Slice 0).
//!
//! Covers the eight cases enumerated in the task spec:
//!   1. Request round-trips through JSON
//!   2. Request with missing `session` field defaults to "main"
//!   3. Response::ok serializes with non-null result and null error
//!   4. Response::error serializes with null result and non-null error
//!   5. parse_request_strict rejects mismatched version with VERSION_MISMATCH
//!   6. parse_request_strict accepts version=1
//!   7. StreamFrame text chunk round-trips
//!   8. StreamFrame with id=None (push frame) serializes without "id" field

use super::protocol::{
    PROTOCOL_VERSION, ProtocolError, Request, Response, StreamFrame, parse_request_strict,
};
use serde_json::json;

#[test]
fn request_round_trips_through_json() {
    let req = Request {
        version: PROTOCOL_VERSION,
        id: "req-1".into(),
        session: "main".into(),
        method: "ping".into(),
        params: json!({"hello": "world"}),
        frontend_capabilities: vec![],
        frontend_extensions: Vec::new(),
    };

    let bytes = serde_json::to_vec(&req).expect("serialize");
    let back: Request = serde_json::from_slice(&bytes).expect("deserialize");

    assert_eq!(back.version, req.version);
    assert_eq!(back.id, req.id);
    assert_eq!(back.session, req.session);
    assert_eq!(back.method, req.method);
    assert_eq!(back.params, req.params);
}

#[test]
fn request_session_defaults_to_main() {
    // Wire payload omits `session` entirely.
    let raw = json!({
        "version": PROTOCOL_VERSION,
        "id": "req-2",
        "method": "ping",
        "params": {}
    });
    let req: Request = serde_json::from_value(raw).expect("deserialize w/o session");
    assert_eq!(req.session, "main");
}

#[test]
fn response_ok_shape() {
    let resp = Response::ok("req-3".into(), json!({"answer": 42}));
    let value = serde_json::to_value(&resp).expect("serialize");

    assert_eq!(value["version"], PROTOCOL_VERSION);
    assert_eq!(value["id"], "req-3");
    assert_eq!(value["result"], json!({"answer": 42}));
    // error must be present-and-null in the JSON object so clients can
    // discriminate result-vs-error by `null`-ness.
    assert!(value.get("error").is_some(), "error field must exist");
    assert!(
        value["error"].is_null(),
        "error must be null on ok responses"
    );
}

#[test]
fn response_error_shape() {
    let err = ProtocolError {
        code: "BOOM".into(),
        message: "kaboom".into(),
        retryable: false,
    };
    let resp = Response::error("req-4".into(), err);
    let value = serde_json::to_value(&resp).expect("serialize");

    assert_eq!(value["version"], PROTOCOL_VERSION);
    assert_eq!(value["id"], "req-4");
    assert!(value.get("result").is_some(), "result field must exist");
    assert!(
        value["result"].is_null(),
        "result must be null on error responses"
    );
    assert_eq!(value["error"]["code"], "BOOM");
    assert_eq!(value["error"]["message"], "kaboom");
    assert_eq!(value["error"]["retryable"], false);
}

#[test]
fn version_mismatch_rejected() {
    let raw = serde_json::to_vec(&json!({
        "version": 999,
        "id": "req-5",
        "session": "main",
        "method": "ping",
        "params": {}
    }))
    .expect("encode");

    let err = parse_request_strict(&raw).expect_err("must reject mismatched version");
    assert_eq!(err.code, "VERSION_MISMATCH");
    assert!(!err.retryable, "version mismatch is not retryable");
}

#[test]
fn version_one_accepted() {
    let raw = serde_json::to_vec(&json!({
        "version": 1,
        "id": "req-6",
        "session": "main",
        "method": "ping",
        "params": {}
    }))
    .expect("encode");

    let req = parse_request_strict(&raw).expect("v1 must parse");
    assert_eq!(req.version, 1);
    assert_eq!(req.id, "req-6");
    assert_eq!(req.method, "ping");
}

#[test]
fn stream_frame_text_chunk_round_trip() {
    let frame = StreamFrame {
        version: PROTOCOL_VERSION,
        id: Some("req-7".into()),
        stream: "text".into(),
        data: json!({"chunk": "hello"}),
    };

    let bytes = serde_json::to_vec(&frame).expect("serialize");
    let back: StreamFrame = serde_json::from_slice(&bytes).expect("deserialize");

    assert_eq!(back.version, frame.version);
    assert_eq!(back.id, frame.id);
    assert_eq!(back.stream, frame.stream);
    assert_eq!(back.data, frame.data);
}

#[test]
fn stream_frame_push_omits_id() {
    let frame = StreamFrame {
        version: PROTOCOL_VERSION,
        id: None,
        stream: "config_reloaded".into(),
        data: json!({}),
    };

    let value = serde_json::to_value(&frame).expect("serialize");
    assert!(
        value.get("id").is_none(),
        "push frames must omit the `id` field entirely, got {value}"
    );
    assert_eq!(value["stream"], "config_reloaded");
    assert_eq!(value["version"], PROTOCOL_VERSION);
}

// -----------------------------------------------------------------------------
// Task 0.3: length-delimited frame I/O over async streams
// -----------------------------------------------------------------------------

use super::protocol::{
    read_frame_bytes, read_request, read_response, read_stream_frame, write_frame_bytes,
    write_request, write_response, write_stream_frame,
};
use tokio::io::duplex;

#[tokio::test]
async fn frame_round_trip_request() {
    let (mut a, mut b) = duplex(4096);
    let req = Request {
        version: 1,
        id: "x".into(),
        session: "main".into(),
        method: "daemon.ping".into(),
        params: serde_json::json!({}),
        frontend_capabilities: vec![],
        frontend_extensions: Vec::new(),
    };
    write_request(&mut a, &req).await.unwrap();
    drop(a); // signal EOF to reader after the frame
    let got = read_request(&mut b).await.unwrap();
    assert_eq!(got.id, "x");
    assert_eq!(got.method, "daemon.ping");
}

#[tokio::test]
async fn frame_round_trip_response() {
    let (mut a, mut b) = duplex(4096);
    let resp = Response::ok("r1".into(), serde_json::json!({"pong": true}));
    write_response(&mut a, &resp).await.unwrap();
    drop(a);
    let got = read_response(&mut b).await.unwrap();
    assert_eq!(got.id, "r1");
    assert_eq!(got.result.unwrap()["pong"], true);
    assert!(got.error.is_none());
}

#[tokio::test]
async fn frame_round_trip_stream_frame() {
    let (mut a, mut b) = duplex(4096);
    let f = StreamFrame {
        version: 1,
        id: Some("s1".into()),
        stream: "text".into(),
        data: serde_json::json!({"chunk": "hi"}),
    };
    write_stream_frame(&mut a, &f).await.unwrap();
    drop(a);
    let got = read_stream_frame(&mut b).await.unwrap();
    assert_eq!(got.id.as_deref(), Some("s1"));
    assert_eq!(got.data["chunk"], "hi");
}

#[tokio::test]
async fn multiple_frames_round_trip_in_order() {
    let (mut a, mut b) = duplex(4096);
    write_frame_bytes(&mut a, b"first").await.unwrap();
    write_frame_bytes(&mut a, b"second").await.unwrap();
    write_frame_bytes(&mut a, b"third").await.unwrap();
    drop(a);
    assert_eq!(read_frame_bytes(&mut b).await.unwrap(), b"first");
    assert_eq!(read_frame_bytes(&mut b).await.unwrap(), b"second");
    assert_eq!(read_frame_bytes(&mut b).await.unwrap(), b"third");
}

#[tokio::test]
async fn oversized_frame_rejected_before_alloc() {
    use tokio::io::AsyncWriteExt;
    let (mut a, mut b) = duplex(64);
    // Header claims 5 MiB; never write a body.
    let huge: u32 = 5 * 1024 * 1024;
    a.write_all(&huge.to_be_bytes()).await.unwrap();
    let result = read_frame_bytes(&mut b).await;
    let err = result.expect_err("must reject oversize");
    assert!(
        err.to_string().contains("MAX_FRAME_BYTES"),
        "error must mention MAX_FRAME_BYTES, got: {err}"
    );
}

#[tokio::test]
async fn eof_mid_header_returns_unexpected_eof() {
    use tokio::io::AsyncWriteExt;
    let (mut a, mut b) = duplex(64);
    // Only 2 of 4 length bytes
    a.write_all(&[0u8, 0u8]).await.unwrap();
    drop(a);
    let err = read_frame_bytes(&mut b).await.expect_err("must error");
    assert_eq!(err.kind(), std::io::ErrorKind::UnexpectedEof);
}

#[tokio::test]
async fn eof_mid_body_returns_unexpected_eof() {
    use tokio::io::AsyncWriteExt;
    let (mut a, mut b) = duplex(64);
    let claimed_len: u32 = 100;
    a.write_all(&claimed_len.to_be_bytes()).await.unwrap();
    a.write_all(b"only ten!!").await.unwrap(); // 10 bytes, claimed 100
    drop(a);
    let err = read_frame_bytes(&mut b).await.expect_err("must error");
    assert_eq!(err.kind(), std::io::ErrorKind::UnexpectedEof);
}

#[tokio::test]
async fn read_request_propagates_version_mismatch() {
    let (mut a, mut b) = duplex(4096);
    let bad = serde_json::json!({
        "version": 99, "id": "x", "session": "main",
        "method": "daemon.ping", "params": {}
    });
    let body = serde_json::to_vec(&bad).unwrap();
    write_frame_bytes(&mut a, &body).await.unwrap();
    drop(a);
    let err = read_request(&mut b).await.expect_err("must error");
    // ProtocolError surfaces inside io::Error::other
    assert!(
        err.to_string().contains("VERSION_MISMATCH"),
        "expected VERSION_MISMATCH, got: {err}"
    );
}

// -----------------------------------------------------------------------------
// Backfill from Task 0.2 review
// -----------------------------------------------------------------------------

#[test]
fn parse_request_strict_rejects_malformed_json() {
    let err =
        super::protocol::parse_request_strict(b"not json").expect_err("must reject malformed JSON");
    assert_eq!(err.code, "INVALID_PARAMS");
    assert!(!err.retryable);
}

#[test]
fn parse_request_strict_rejects_valid_json_wrong_shape() {
    // Valid JSON, but an array — not a Request struct
    let err =
        super::protocol::parse_request_strict(b"[1, 2, 3]").expect_err("must reject wrong shape");
    assert_eq!(err.code, "INVALID_PARAMS");
}

#[test]
fn protocol_error_display_format() {
    let err = super::protocol::ProtocolError {
        code: "FOO".into(),
        message: "bar baz".into(),
        retryable: false,
    };
    assert_eq!(format!("{err}"), "FOO: bar baz");
}

#[test]
fn response_error_round_trips_through_json() {
    let resp = super::protocol::Response::error(
        "id-1".into(),
        super::protocol::ProtocolError {
            code: "VERSION_MISMATCH".into(),
            message: "client v2, daemon v1".into(),
            retryable: false,
        },
    );
    let bytes = serde_json::to_vec(&resp).unwrap();
    let parsed: super::protocol::Response = serde_json::from_slice(&bytes).unwrap();
    let err = parsed.error.expect("error preserved");
    assert_eq!(err.code, "VERSION_MISMATCH");
    assert_eq!(err.message, "client v2, daemon v1");
    assert!(parsed.result.is_none());
}
