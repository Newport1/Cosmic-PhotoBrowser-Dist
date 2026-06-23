/// Clicking star `clicked` (1..=5): if it equals the current rating, clear to 0 (unrated);
/// otherwise set to `clicked`. Pure.
pub(crate) fn next_rating_on_click(current: Option<u8>, clicked: u8) -> u8 {
    if current == Some(clicked) {
        0
    } else {
        clicked.clamp(0, 5)
    }
}

/// Pure summary formatter for batch cull status (extracted for easy unit test).
pub(crate) fn batch_summary(ok: usize, total: usize, verb: &str) -> String {
    if ok == total {
        format!("{verb} {ok} item(s)")
    } else {
        format!("{verb} {ok}/{total} ({} failed)", total - ok)
    }
}

/// Cycle the rating filter: None -> 1 -> 2 -> 3 -> 4 -> 5 -> None.
pub(crate) fn next_rating_filter(cur: Option<u8>) -> Option<u8> {
    match cur {
        None => Some(1),
        Some(n) if n >= 5 => None,
        Some(n) => Some(n + 1),
    }
}

/// Cycle the label filter: None -> Red -> Yellow -> Green -> Blue -> Purple -> None.
/// Uses the canonical order from ColorLabel::all().
pub(crate) fn next_label_filter(
    cur: Option<crate::xmp::ColorLabel>,
) -> Option<crate::xmp::ColorLabel> {
    let all = crate::xmp::ColorLabel::all();
    match cur {
        None => Some(all[0]),
        Some(cur) => {
            if let Some(pos) = all.iter().position(|&c| c == cur) {
                let next = (pos + 1) % all.len();
                if next == 0 {
                    None
                } else {
                    Some(all[next])
                }
            } else {
                None
            }
        }
    }
}

/// Per-item cull filter predicate. Pure for testability.
/// - If hide_rejected and meta.rejected: false
/// - If rating_min=Some(m): require meta.rating.unwrap_or(0) >= m && !meta.rejected
/// - If label=Some(l): require meta.label == Some(l)
/// - Otherwise pass (subject to above)
pub(crate) fn cull_passes(
    meta: crate::xmp::CullMeta,
    rating_min: Option<u8>,
    label: Option<crate::xmp::ColorLabel>,
    hide_rejected: bool,
) -> bool {
    if hide_rejected && meta.rejected {
        return false;
    }
    if let Some(min) = rating_min {
        if meta.rating.unwrap_or(0) < min || meta.rejected {
            return false;
        }
    }
    if let Some(l) = label {
        if meta.label != Some(l) {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    // Access the moved items via super (matching the style used for these tests in app.rs).
    // batch_summary calls updated from bare (which relied on a use super import) to super::.

    #[test]
    fn next_rating_on_click_toggle_and_set() {
        // Pure; clicking current rating clears to 0; otherwise sets (clamped).
        // Representative rating transitions.
        assert_eq!(super::next_rating_on_click(Some(3), 3), 0);
        assert_eq!(super::next_rating_on_click(Some(2), 4), 4);
        assert_eq!(super::next_rating_on_click(None, 5), 5);
        assert_eq!(super::next_rating_on_click(Some(0), 3), 3);
    }

    #[test]
    fn next_rating_filter_cycles_none_to_1_to_5_then_none() {
        // The filter cycles from no rating through one to five stars and back.
        assert_eq!(super::next_rating_filter(None), Some(1));
        assert_eq!(super::next_rating_filter(Some(1)), Some(2));
        assert_eq!(super::next_rating_filter(Some(5)), None);
        assert_eq!(super::next_rating_filter(Some(4)), Some(5));
    }

    #[test]
    fn next_label_filter_cycles_none_red_yellow_green_blue_purple_none() {
        use crate::xmp::ColorLabel;
        assert_eq!(super::next_label_filter(None), Some(ColorLabel::Red));
        assert_eq!(
            super::next_label_filter(Some(ColorLabel::Red)),
            Some(ColorLabel::Yellow)
        );
        assert_eq!(
            super::next_label_filter(Some(ColorLabel::Yellow)),
            Some(ColorLabel::Green)
        );
        assert_eq!(
            super::next_label_filter(Some(ColorLabel::Green)),
            Some(ColorLabel::Blue)
        );
        assert_eq!(
            super::next_label_filter(Some(ColorLabel::Blue)),
            Some(ColorLabel::Purple)
        );
        assert_eq!(super::next_label_filter(Some(ColorLabel::Purple)), None);
        assert_eq!(
            super::next_label_filter(Some(ColorLabel::Red)),
            Some(ColorLabel::Yellow)
        ); // idempotent step
    }

    #[test]
    fn cull_passes_rejected_label_hide_and_all_cases() {
        use crate::xmp::{ColorLabel, CullMeta};
        let rej = CullMeta {
            rating: None,
            label: None,
            rejected: true,
        };
        let rated3_red = CullMeta {
            rating: Some(3),
            label: Some(ColorLabel::Red),
            rejected: false,
        };
        let rated0_yellow = CullMeta {
            rating: Some(0),
            label: Some(ColorLabel::Yellow),
            rejected: false,
        };
        let unrated_green = CullMeta {
            rating: None,
            label: Some(ColorLabel::Green),
            rejected: false,
        };
        let rejected_red = CullMeta {
            rating: None,
            label: Some(ColorLabel::Red),
            rejected: true,
        };

        // rejected photo fails rating_min=Some(1)
        assert!(!super::cull_passes(rej, Some(1), None, false));
        assert!(!super::cull_passes(rejected_red, Some(1), None, false));

        // label filter keeps only matching label
        assert!(super::cull_passes(
            rated3_red,
            None,
            Some(ColorLabel::Red),
            false
        ));
        assert!(!super::cull_passes(
            rated3_red,
            None,
            Some(ColorLabel::Yellow),
            false
        ));
        assert!(super::cull_passes(
            unrated_green,
            None,
            Some(ColorLabel::Green),
            false
        ));

        // hide_rejected drops rejected
        assert!(!super::cull_passes(rej, None, None, true));
        assert!(!super::cull_passes(rejected_red, None, None, true));
        assert!(super::cull_passes(rated0_yellow, None, None, true));

        // None/None/false keeps everything (non-rej)
        assert!(super::cull_passes(rated3_red, None, None, false));
        assert!(super::cull_passes(rated0_yellow, None, None, false));
        assert!(super::cull_passes(unrated_green, None, None, false));
        // even rejected passes if not hiding and no other filters (per current rating semantics for "all")
        assert!(super::cull_passes(rej, None, None, false));
    }

    #[test]
    fn batch_summary_formats_ok_and_partial() {
        assert_eq!(super::batch_summary(3, 3, "Rated"), "Rated 3 item(s)");
        assert_eq!(
            super::batch_summary(2, 5, "Labeled"),
            "Labeled 2/5 (3 failed)"
        );
        assert_eq!(
            super::batch_summary(0, 2, "Rejected"),
            "Rejected 0/2 (2 failed)"
        );
        assert_eq!(
            super::batch_summary(1, 1, "Cleared reject on"),
            "Cleared reject on 1 item(s)"
        );
    }
}
