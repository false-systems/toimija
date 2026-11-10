//! The canonical form and its digest, pinned.
//!
//! The canonical form of a JSON value is compact JSON with object keys sorted
//! at every level and arrays in order; the digest is SHA-256 over those bytes.
//! A seal is only checkable by another reader while both stay exactly this, so
//! each case below pins the bytes and the digest. A change here is a change to
//! every digest Toimija has written.

use serde_json::json;
use toimija::seal::{canonical_json, digest_json, digest_value};

fn cases() -> Vec<(serde_json::Value, &'static str, &'static str)> {
    vec![
        (
            json!({
                "z": "é\n\"q\"\t\\",
                "a": [true, null, {"k": -1, "b": 0}],
                "m": {"y": 18446744073709551615u64, "x": -9223372036854775808i64},
            }),
            r#"{"a":[true,null,{"b":0,"k":-1}],"m":{"x":-9223372036854775808,"y":18446744073709551615},"z":"é\n\"q\"\t\\"}"#,
            "sha256:de3011d1a038cc9bd1edca841531f3865d5d81f658cbafd684bc262627088b81",
        ),
        (
            json!({}),
            "{}",
            "sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a",
        ),
        (
            json!([[], {}, "", 0]),
            r#"[[],{},"",0]"#,
            "sha256:9a287567bbd4977bac08f09b522d1ed8a47e7f4d7ac22223a409d5f317a0c777",
        ),
        (
            json!("plain text / with ünïcode ✓"),
            r#""plain text / with ünïcode ✓""#,
            "sha256:03595ac576015655e2ab96591ac48889ee751dea80bef30827f6998040ee075b",
        ),
        (
            json!({"schema": "demo.v1", "paths": ["src/a b.rs", {"bytes_hex": "66ff6f"}], "count": 3}),
            r#"{"count":3,"paths":["src/a b.rs",{"bytes_hex":"66ff6f"}],"schema":"demo.v1"}"#,
            "sha256:54da050cf282994f81ce9479d5257f3a0b00e3d0153d6b560b2c30aec5ded9c0",
        ),
    ]
}

#[test]
fn canonical_bytes_and_digests_are_pinned() {
    for (value, canonical, digest) in cases() {
        assert_eq!(canonical_json(&value), canonical, "{value}");
        assert_eq!(digest_value(&value), digest, "{value}");
        assert_eq!(digest_json(canonical), digest, "{value}");
    }
}
