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
