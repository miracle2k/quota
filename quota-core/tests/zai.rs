//! Covers reading Z.ai's Coding Plan quota and account balance.

use quota_core::zai::{parse_balance, parse_quota, Quota, Region};
use serde_json::json;

#[test]
fn reads_the_plan_windows_shortest_first() {
    let raw = json!({
        "code": 200, "msg": "success", "success": true,
        "data": {"planName": "Pro", "limits": [
            {"type": "TOKENS_LIMIT", "unit": 6, "number": 1, "percentage": 9, "nextResetTime": 1786291200000i64},
            {"type": "TOKENS_LIMIT", "unit": 3, "number": 5, "percentage": 25, "nextResetTime": 1785816000000i64},
            {"type": "TIME_LIMIT", "unit": 5, "number": 1, "usage": 1000, "currentValue": 224,
             "remaining": 776, "percentage": 22}
        ]}
    });
    let Ok(Quota::Plan { name, windows }) = parse_quota(&raw) else {
        panic!("plan");
    };
    assert_eq!(name.as_deref(), Some("Pro"));
    let summary: Vec<(String, Option<i32>, Option<i64>)> = windows
        .into_iter()
        .map(|window| (window.label, window.remaining_percent, window.reset_at))
        .collect();
    assert_eq!(
        summary,
        vec![
            ("5h".to_string(), Some(75), Some(1_785_816_000)),
            ("Wk".to_string(), Some(91), Some(1_786_291_200)),
            // The counts win over the rounded percentage: 224 of 1000 used.
            ("MCP".to_string(), Some(78), None),
        ]
    );
}

#[test]
fn an_account_without_a_coding_plan_is_told_apart_from_an_error() {
    let no_plan = json!({"code": 500, "msg": "当前用户不存在coding plan", "success": false});
    assert_eq!(parse_quota(&no_plan).expect("no plan"), Quota::NoPlan);

    let failure = json!({"code": 1001, "msg": "Authorization failure", "success": false});
    let error = parse_quota(&failure).expect_err("error");
    assert!(error.to_string().contains("Authorization failure"));
}

#[test]
fn reads_the_balance_in_the_region_currency() {
    let raw = json!({"code": 200, "msg": "Operation successful", "success": true,
        "data": {"availableBalance": 7.94901492, "balance": 7.94901492, "rechargeAmount": 105.0}});
    let global = parse_balance(&raw, Region::Global).expect("balance");
    assert_eq!(global.currency, "USD");
    assert_eq!(global.display(), "$7.95");
    assert_eq!(
        parse_balance(&raw, Region::China)
            .expect("balance")
            .display(),
        "¥7.95"
    );
    assert_eq!(
        parse_balance(&json!({"success": false}), Region::Global),
        None
    );
}
