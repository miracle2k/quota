use quota_core::codex::local_usage::normalize_usage_response;
use serde_json::json;

#[test]
fn inverts_used_percent_into_remaining_percent() {
    // The Codex API reports how much of each window has been used. Everything
    // downstream, including the sidebar token, reads remaining. If this test
    // fails, the sidebar is telling users the opposite of the truth.
    let raw = json!({
        "plan_type": "pro",
        "rate_limit": {
            "primary_window": { "used_percent": 9, "limit_window_seconds": 18000 },
            "secondary_window": { "used_percent": 37, "limit_window_seconds": 604800 }
        }
    });

    let usage = normalize_usage_response(&raw, None);

    assert_eq!(usage.provider, "codex");
    assert_eq!(usage.account_label.as_deref(), Some("pro"));
    assert_eq!(usage.windows[0].remaining_percent, Some(91));
    assert_eq!(usage.windows[1].remaining_percent, Some(63));
}

#[test]
fn derives_window_labels_from_the_reported_window_length() {
    let raw = json!({
        "rate_limit": {
            "primary_window": { "used_percent": 0, "limit_window_seconds": 18000 },
            "secondary_window": { "used_percent": 0, "limit_window_seconds": 604800 }
        }
    });

    let usage = normalize_usage_response(&raw, None);

    assert_eq!(usage.windows[0].label, "5h");
    assert_eq!(usage.windows[1].label, "Wk");
}

#[test]
fn an_odd_window_length_keeps_the_fallback_label() {
    // 437 minutes is neither a whole hour nor a whole day. Inventing a label
    // for it would read worse than the default.
    let raw = json!({
        "rate_limit": {
            "primary_window": { "used_percent": 10, "limit_window_seconds": 26220 }
        }
    });

    let usage = normalize_usage_response(&raw, None);

    assert_eq!(usage.windows[0].label, "5h");
}

#[test]
fn renders_the_compact_token_the_sidebar_shows() {
    let raw = json!({
        "rate_limit": {
            "primary_window": { "used_percent": 9, "limit_window_seconds": 18000 },
            "secondary_window": { "used_percent": 37, "limit_window_seconds": 604800 }
        }
    });

    let usage = normalize_usage_response(&raw, None);

    assert_eq!(usage.compact_token().as_deref(), Some("5h 91% · Wk 63%"));
}

#[test]
fn a_missing_rate_limit_block_produces_windows_with_no_numbers() {
    let usage = normalize_usage_response(&json!({ "plan_type": "plus" }), None);

    assert_eq!(usage.windows.len(), 2);
    assert!(usage.windows.iter().all(|w| w.remaining_percent.is_none()));
    assert_eq!(usage.compact_token(), None);
}

#[test]
fn a_fully_used_window_reports_zero_rather_than_a_negative() {
    let raw = json!({
        "rate_limit": {
            "primary_window": { "used_percent": 140, "limit_window_seconds": 18000 }
        }
    });

    let usage = normalize_usage_response(&raw, None);

    assert_eq!(usage.windows[0].remaining_percent, Some(0));
}

#[test]
fn an_absolute_reset_timestamp_is_used_as_given() {
    let raw = json!({
        "rate_limit": {
            "primary_window": {
                "used_percent": 5,
                "limit_window_seconds": 18000,
                "reset_at": 1_789_041_497i64,
                "reset_after_seconds": 60
            }
        }
    });

    let usage = normalize_usage_response(&raw, None);

    assert_eq!(usage.windows[0].reset_at, Some(1_789_041_497));
}

#[test]
fn a_relative_reset_is_turned_into_a_timestamp() {
    let raw = json!({
        "rate_limit": {
            "primary_window": {
                "used_percent": 5,
                "limit_window_seconds": 18000,
                "reset_after_seconds": 3600
            }
        }
    });

    let now = chrono::Utc::now().timestamp();
    let usage = normalize_usage_response(&raw, None);
    let reset = usage.windows[0].reset_at.expect("reset timestamp");

    assert!(
        (reset - (now + 3600)).abs() <= 5,
        "expected roughly an hour from now, got {} against {}",
        reset,
        now + 3600
    );
}

#[test]
fn an_explicit_account_label_wins_over_the_payload_plan() {
    let raw = json!({ "plan_type": "plus" });

    let usage = normalize_usage_response(&raw, Some("pro".to_string()));

    assert_eq!(usage.account_label.as_deref(), Some("pro"));
}

#[test]
fn reads_the_banked_reset_count() {
    let raw = json!({
        "rate_limit": {
            "primary_window": { "used_percent": 100, "limit_window_seconds": 18000 }
        },
        "rate_limit_reset_credits": { "available_count": 2, "applicable_available_count": 1 }
    });

    let resets = normalize_usage_response(&raw, None).resets.expect("resets");

    assert_eq!(resets.available, 2);
    // The count comes without dates; those need a separate call.
    assert_eq!(resets.expires_at, None);
}

#[test]
fn a_payload_without_reset_credits_carries_no_reset_information() {
    let raw = json!({
        "rate_limit": {
            "primary_window": { "used_percent": 10, "limit_window_seconds": 18000 }
        }
    });

    assert_eq!(normalize_usage_response(&raw, None).resets, None);
}

#[test]
fn a_login_is_identified_by_workspace_and_email() {
    // Two people can share one workspace, so the account id alone would merge
    // them. Email case differs between sources, so it is compared lowercased.
    let credentials = quota_core::codex::local::LocalCodexCredentials {
        access_token: "token".to_string(),
        account_id: Some("acct-1".to_string()),
        email: Some("Someone@Example.com".to_string()),
        plan: None,
        access_token_expires_at_ms: None,
    };
    assert_eq!(
        credentials.identity().as_deref(),
        Some("acct-1:someone@example.com")
    );

    let anonymous = quota_core::codex::local::LocalCodexCredentials {
        access_token: "token".to_string(),
        account_id: None,
        email: None,
        plan: None,
        access_token_expires_at_ms: None,
    };
    assert_eq!(anonymous.identity(), None);
}

#[test]
fn counts_usable_reset_credits_and_finds_the_soonest_expiry() {
    use quota_core::codex::local_usage::reset_credits_from;

    let now = 1_790_000_000; // 2026-09-21
    let raw = serde_json::json!({
        "available_count": 4,
        "credits": [
            {"id": "a", "status": "available", "expires_at": "2026-11-20T00:00:00Z"},
            {"id": "b", "status": "available", "expires_at": "2026-10-01T00:00:00Z"},
            {"id": "c", "status": "available", "expires_at": null},
            {"id": "d", "status": "redeemed", "expires_at": "2026-09-30T00:00:00Z"},
            {"id": "e", "status": "available", "expires_at": "2026-09-01T00:00:00Z"}
        ]
    });
    let resets = reset_credits_from(&raw, now).expect("resets");
    assert_eq!(resets.available, 3);
    assert_eq!(resets.expires_at, Some(1_790_812_800)); // 2026-10-01

    assert_eq!(reset_credits_from(&serde_json::json!({}), now), None);
}

#[test]
fn reads_the_expiry_of_an_access_token() {
    use quota_core::codex::local::jwt_expiry;

    // {"exp":1800000000}
    let token = "eyJhbGciOiJub25lIn0.eyJleHAiOjE4MDAwMDAwMDB9.sig";
    assert_eq!(
        jwt_expiry(token).map(|at| at.timestamp()),
        Some(1_800_000_000)
    );
    assert_eq!(jwt_expiry("not-a-jwt"), None);
}

#[test]
fn codex_treats_the_same_refresh_errors_as_final_as_the_cli_does() {
    use quota_core::codex::refresh::classify_failure;

    for body in [
        r#"{"error":"refresh_token_reused"}"#,
        r#"{"error":{"code":"refresh_token_expired"}}"#,
        r#"{"code":"refresh_token_invalidated"}"#,
    ] {
        assert!(classify_failure(400, body).needs_sign_in(), "{}", body);
    }
    assert!(classify_failure(400, r#"{"error":"invalid_grant"}"#).needs_sign_in());
    assert!(classify_failure(401, "").needs_sign_in());
    assert!(!classify_failure(500, r#"{"error":"server_error"}"#).needs_sign_in());
}
