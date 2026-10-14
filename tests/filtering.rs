use fastcull::{
    arw::CaptureMetadata,
    filtering::{
        Bounds, Comparison, Field, Filters, Row, reconcile_visible, step_visible, time_key,
    },
    xmp::Values,
};
use std::path::Path;

#[test]
fn bridge_or_within_and_between_fields_and_dynamic_counts() {
    let values: Vec<_> = [(4, "Red"), (5, "Blue"), (3, "Red"), (4, "Green")]
        .into_iter()
        .map(|(rating, label)| Values {
            rating: Some(rating),
            label: Some(label.into()),
            ..Default::default()
        })
        .collect();
    let rows: Vec<_> = values
        .iter()
        .map(|v| Row {
            path: Path::new("照片.ARW"),
            values: v,
            capture: None,
            indexed: false,
        })
        .collect();
    let mut f = Filters::default();
    f.selected
        .insert(Field::Rating, [Some("4".into()), Some("5".into())].into());
    f.selected.insert(
        Field::Label,
        [Some("red".into()), Some("blue".into())].into(),
    );
    assert_eq!(
        rows.iter().map(|r| f.matches(r, None)).collect::<Vec<_>>(),
        [true, true, false, false]
    );
    let counts = f.facets(&rows, Field::Label);
    assert_eq!(counts[&Some("green".into())], 1); // Ignore this facet only.
    f.selected.insert(Field::Rating, [Some("4".into())].into());
    f.comparison = Comparison::AtLeast;
    assert!(f.matches(&rows[1], None));
    f.comparison = Comparison::AtMost;
    assert!(f.matches(&rows[2], None));
}
#[test]
fn capture_ranges_are_inclusive_and_pending_is_not_unknown() {
    let v = Values::default();
    let c = CaptureMetadata {
        captured_at: Some("2026:10:12 23:59:59".into()),
        iso: Some(5000),
        shutter: Some(1. / 250.),
        ..Default::default()
    };
    let mut row = Row {
        path: Path::new("IMG 中文.ARW"),
        values: &v,
        capture: Some(&c),
        indexed: true,
    };
    let mut f = Filters {
        query: "img 中文".into(),
        date_from: time_key("2026-10-12", false),
        date_to: time_key("2026-10-12", true),
        ..Default::default()
    };
    f.ranges.insert(
        Field::Iso,
        Bounds {
            min: Some(5000.),
            max: Some(5000.),
        },
    );
    f.ranges.insert(
        Field::Shutter,
        Bounds {
            min: Some(1. / 500.),
            max: Some(1. / 250.),
        },
    );
    assert!(f.matches(&row, None));
    row.indexed = false;
    assert!(!f.matches(&row, None));
    f = Filters::default();
    f.selected.insert(Field::Lens, [None].into());
    assert!(!f.matches(&row, None));
    row.indexed = true;
    assert!(f.matches(&row, None));
    f.ranges.insert(
        Field::Focal,
        Bounds {
            min: Some(1.),
            max: None,
        },
    );
    assert!(!f.matches(&row, None));
    assert!(time_key("2024-02-29", false).is_some());
    for s in [
        "2026-02-29",
        "2026-10-12 24:00:00",
        "0000-01-01",
        "坏日期",
        "2026-13-01",
    ] {
        assert!(time_key(s, false).is_none(), "{s}");
    }
}
#[test]
fn filtering_preserves_original_photo_identity_and_empty_results() {
    let visible = [1, 4, 9];
    assert_eq!(step_visible(&visible, 4, 1), Some(9));
    assert_eq!(step_visible(&visible, 4, -1), Some(1));
    assert_eq!(reconcile_visible(&visible, 5), Some(9));
    assert_eq!(reconcile_visible(&visible, 10), Some(9));
    assert_eq!(reconcile_visible(&[], 5), None);
    assert_eq!(step_visible(&visible, 3, 1), None);
}
