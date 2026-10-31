use quota_core::claude::local_usage::normalize_usage_response;
use serde_json::json;

#[test]
fn normalizes_five_hour_and_weekly_into_ordered_windows() {
    let raw = json!({
        "five_hour": {
            "utilization": 11.0,
            "resets_at": "2026-09-10T09:59:59.709197+00:00"
        },
        "seven_day": {
            "utilization": 43.0,
            "resets_at": "2026-09-14T10:59:59.709221+00:00"
        }
    });

    let usage = normalize_usage_response(&raw, Some("pro".to_string()));

    assert_eq!(usage.provider, "claude");
    assert_eq!(usage.account_label.as_deref(), Some("pro"));
    assert_eq!(usage.windows.len(), 2);
    assert_eq!(usage.windows[0].label, "5h");
    assert_eq!(usage.windows[0].remaining_percent, Some(89));
    assert_eq!(usage.windows[1].label, "Wk");
    assert_eq!(usage.windows[1].remaining_percent, Some(57));
}

#[test]
fn renders_the_compact_token_the_sidebar_shows() {
    let raw = json!({
        "five_hour": { "utilization": 11.0 },
        "seven_day": { "utilization": 43.0 }
    });

    let usage = normalize_usage_response(&raw, None);
    assert_eq!(usage.compact_token().as_deref(), Some("5h 89% · Wk 57%"));
}

#[test]
fn a_missing_block_produces_a_window_with_no_number() {
    let raw = json!({ "five_hour": { "utilization": 11.0 } });

    let usage = normalize_usage_response(&raw, None);
    assert_eq!(usage.windows[1].label, "Wk");
    assert_eq!(usage.windows[1].remaining_percent, None);
    assert_eq!(usage.compact_token().as_deref(), Some("5h 89%"));
}

#[test]
fn extra_model_blocks_are_ignored() {
    let raw = json!({
        "five_hour": { "utilization": 11.0 },
        "seven_day": { "utilization": 43.0 },
        "seven_day_opus": { "utilization": 88.0 },
        "seven_day_sonnet": { "utilization": 2.0 }
    });

    let usage = normalize_usage_response(&raw, None);
    assert_eq!(usage.windows.len(), 2);
    assert_eq!(usage.compact_token().as_deref(), Some("5h 89% · Wk 57%"));
}

#[test]
fn sums_banked_limit_resets_and_keeps_the_soonest_expiry() {
    // Each grant has its own count and end date. The report shows the total,
    // and the end date that runs out first, because that is the deadline the
    // user has to act on.
    let raw = json!({
        "five_hour": null,
        "seven_day": { "utilization": 1.0, "resets_at": "2026-11-06T14:00:00+00:00" },
        "cedar_ember": {
            "eligible": true,
            "grants": [
                { "id": "g1", "resets_total": 2, "resets_left": 1, "ends_at": "2026-12-01T00:00:00Z" },
                { "id": "g2", "resets_total": 3, "resets_left": 2, "ends_at": "2026-11-20T00:00:00Z" },
                { "id": "g3", "resets_total": 1, "resets_left": 0, "ends_at": "2026-11-10T00:00:00Z" }
            ]
        }
    });

    let resets = normalize_usage_response(&raw, None).resets.expect("resets");

    assert_eq!(resets.available, 3);
    // A spent grant's end date is not a deadline for anything.
    assert_eq!(resets.expires_at, Some(1_795_132_800));
}

#[test]
fn an_account_without_grants_reports_zero_resets() {
    let raw = json!({
        "cedar_ember": { "eligible": false, "ineligible_reason": "no_grant", "grants": [] }
    });

    let resets = normalize_usage_response(&raw, None).resets.expect("resets");

    assert_eq!(resets.available, 0);
    assert_eq!(resets.expires_at, None);
}

#[test]
fn resets_withheld_from_a_client_that_is_not_claude_code_are_unknown_not_zero() {
    // The API answers "surface" when the user agent does not look like Claude
    // Code. That says nothing about the account, so it must not read as zero.
    let raw = json!({
        "cedar_ember": { "eligible": false, "ineligible_reason": "surface", "grants": [] }
    });

    assert_eq!(normalize_usage_response(&raw, None).resets, None);
}

#[test]
fn a_null_or_missing_reset_block_carries_no_reset_information() {
    // The plain usage call returns the block as null.
    assert_eq!(
        normalize_usage_response(&json!({ "cedar_ember": null }), None).resets,
        None
    );
    assert_eq!(normalize_usage_response(&json!({}), None).resets, None);
}
