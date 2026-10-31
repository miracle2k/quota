//! Covers the text report's window rendering, including the reset durations
//! the Herdr sidebar token deliberately leaves out.

use quota_cli::accounts::AccountReport;
use quota_cli::render::{account_name, shown_in_text, usage_detail, usage_record};
use quota_core::providers::ProviderError;
use quota_core::usage::{LimitResets, ProviderUsage, UsageWindow};

const NOW: i64 = 1_757_500_000;

fn window(label: &str, percent: Option<i32>, reset_at: Option<i64>) -> UsageWindow {
    UsageWindow {
        label: label.to_string(),
        remaining_percent: percent,
        reset_at,
    }
}

fn usage(windows: Vec<UsageWindow>, note: Option<&str>) -> ProviderUsage {
    ProviderUsage {
        provider: "claude".to_string(),
        account_label: None,
        windows,
        note: note.map(str::to_string),
        resets: None,
    }
}

#[test]
fn a_window_with_a_reset_shows_how_long_is_left() {
    let report = usage(
        vec![window("5h", Some(47), Some(NOW + 2 * 3600 + 14 * 60))],
        None,
    );
    assert_eq!(
        usage_detail(&report, NOW).as_deref(),
        Some("5h 47% left (resets in 2h 14m)")
    );
}

#[test]
fn a_window_without_a_reset_shows_only_the_percent() {
    let report = usage(vec![window("Credits", Some(100), None)], None);
    assert_eq!(usage_detail(&report, NOW).as_deref(), Some("Credits 100% left"));
}

#[test]
fn every_window_is_rendered_not_just_the_first_two() {
    let report = usage(
        vec![
            window("5h", Some(47), None),
            window("Wk", Some(36), None),
            window("Mo", Some(80), None),
        ],
        None,
    );
    assert_eq!(
        usage_detail(&report, NOW).as_deref(),
        Some("5h 47% left · Wk 36% left · Mo 80% left")
    );
}

#[test]
fn a_reset_further_out_than_a_day_reads_in_days_and_hours() {
    let report = usage(
        vec![window("Wk", Some(36), Some(NOW + 6 * 86_400 + 3 * 3600))],
        None,
    );
    assert_eq!(
        usage_detail(&report, NOW).as_deref(),
        Some("Wk 36% left (resets in 6d 3h)")
    );
}

#[test]
fn a_whole_number_of_hours_drops_the_minutes() {
    let report = usage(vec![window("5h", Some(10), Some(NOW + 3 * 3600))], None);
    assert_eq!(
        usage_detail(&report, NOW).as_deref(),
        Some("5h 10% left (resets in 3h)")
    );
}

#[test]
fn a_whole_number_of_days_drops_the_hours() {
    let report = usage(vec![window("Wk", Some(10), Some(NOW + 2 * 86_400))], None);
    assert_eq!(
        usage_detail(&report, NOW).as_deref(),
        Some("Wk 10% left (resets in 2d)")
    );
}

#[test]
fn less_than_a_minute_left_still_reads_as_a_minute_not_zero() {
    let report = usage(vec![window("5h", Some(3), Some(NOW + 20))], None);
    assert_eq!(
        usage_detail(&report, NOW).as_deref(),
        Some("5h 3% left (resets in 1m)")
    );
}

#[test]
fn a_reset_already_passed_reads_as_due_rather_than_negative() {
    let report = usage(vec![window("5h", Some(0), Some(NOW - 600))], None);
    assert_eq!(
        usage_detail(&report, NOW).as_deref(),
        Some("5h 0% left (resets now)")
    );
}

#[test]
fn a_window_with_no_percent_is_skipped_entirely() {
    let report = usage(
        vec![
            window("Credit", None, Some(NOW + 3600)),
            window("Wk", Some(50), None),
        ],
        None,
    );
    assert_eq!(usage_detail(&report, NOW).as_deref(), Some("Wk 50% left"));
}

#[test]
fn an_account_with_nothing_to_measure_falls_back_to_its_note() {
    let report = usage(
        vec![window("Credit", None, None)],
        Some("no credit allocation"),
    );
    assert_eq!(
        usage_detail(&report, NOW).as_deref(),
        Some("no credit allocation")
    );
}

#[test]
fn nothing_to_report_and_no_note_renders_nothing() {
    let report = usage(vec![window("Credit", None, None)], None);
    assert_eq!(usage_detail(&report, NOW), None);
}

fn with_resets(usage: ProviderUsage, available: u32, expires_at: Option<i64>) -> ProviderUsage {
    ProviderUsage {
        resets: Some(LimitResets {
            available,
            expires_at,
        }),
        ..usage
    }
}

#[test]
fn banked_resets_follow_the_windows() {
    let report = with_resets(
        usage(
            vec![window("5h", Some(0), Some(NOW + 2 * 3600 + 14 * 60))],
            None,
        ),
        2,
        Some(NOW + 6 * 86_400 + 4 * 3600),
    );
    assert_eq!(
        usage_detail(&report, NOW).as_deref(),
        Some("5h 0% left (resets in 2h 14m) · 2 resets available (next expires in 6d 4h)")
    );
}

#[test]
fn a_single_reset_without_a_date_reads_in_the_singular() {
    let report = with_resets(usage(vec![window("Wk", Some(40), None)], None), 1, None);
    assert_eq!(
        usage_detail(&report, NOW).as_deref(),
        Some("Wk 40% left · 1 reset available")
    );
}

#[test]
fn zero_resets_add_nothing_to_the_line() {
    let report = with_resets(usage(vec![window("Wk", Some(40), None)], None), 0, None);
    assert_eq!(usage_detail(&report, NOW).as_deref(), Some("Wk 40% left"));
}

#[test]
fn a_label_from_the_config_file_follows_the_provider_name() {
    assert_eq!(account_name("codex", Some("work")), "codex (work)");
    assert_eq!(account_name("codex", None), "codex");
}

fn report(label: Option<&str>, result: Result<ProviderUsage, ProviderError>) -> AccountReport {
    AccountReport {
        provider: "cursor",
        label: label.map(str::to_string),
        result,
    }
}

#[test]
fn the_text_report_leaves_out_providers_that_are_not_set_up_unless_asked() {
    let missing = report(
        None,
        Err(ProviderError::new("No Cursor credentials found.", true)),
    );
    assert!(!shown_in_text(&missing, NOW, false));
    assert!(shown_in_text(&missing, NOW, true));

    let blank = report(None, Ok(usage(vec![window("Plan", None, None)], None)));
    assert!(!shown_in_text(&blank, NOW, false));

    // A real failure is worth seeing, and so is anything the file asked for.
    let failed = report(
        None,
        Err(ProviderError::new("Cursor rejected the token.", false)),
    );
    assert!(shown_in_text(&failed, NOW, false));

    let configured = report(
        Some("work"),
        Err(ProviderError::new("No login in ~/x.", true)),
    );
    assert!(shown_in_text(&configured, NOW, false));
}

#[test]
fn json_records_keep_every_field_and_say_why_a_login_has_no_numbers() {
    let missing = report(
        None,
        Err(ProviderError::new("No Cursor credentials found.", true)),
    );
    let value = serde_json::to_value(usage_record(&missing)).expect("json");
    assert_eq!(
        value,
        serde_json::json!({
            "provider": "cursor",
            "label": null,
            "status": "not_configured",
            "accountLabel": null,
            "windows": [],
            "note": null,
            "resets": null,
            "error": "No Cursor credentials found."
        })
    );

    let working = report(
        Some("work"),
        Ok(with_resets(
            usage(vec![window("5h", Some(80), Some(NOW))], None),
            2,
            None,
        )),
    );
    let value = serde_json::to_value(usage_record(&working)).expect("json");
    assert_eq!(value["status"], "ok");
    assert_eq!(value["label"], "work");
    assert_eq!(value["windows"][0]["remainingPercent"], 80);
    assert_eq!(
        value["resets"],
        serde_json::json!({ "available": 2, "expiresAt": null })
    );
    assert_eq!(value["error"], serde_json::Value::Null);
}
