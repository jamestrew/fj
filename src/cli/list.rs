use anyhow::Result;
use serde::Serialize;

use crate::client::Page;
use crate::output;

pub fn print_json<T: Serialize>(page: &Page<T>) -> Result<()> {
    output::print_json(&json_value(page))
}

fn json_value<T: Serialize>(page: &Page<T>) -> serde_json::Value {
    serde_json::json!({
        "total_count": page.total,
        "items": page.items,
    })
}

pub fn print_truncation_footer<T>(page: &Page<T>, limit: u32) {
    if truncation_footer_needed(page, limit) {
        let returned = page.items.len();
        let total = page.total.expect("checked by truncation_footer_needed");
        println!(
            "{}",
            output::dim(&format!(
                "Showing {returned} of {total}. Use --limit or --paginate for more."
            ))
        );
    }
}

fn truncation_footer_needed<T>(page: &Page<T>, limit: u32) -> bool {
    let returned = page.items.len();
    page.total
        .is_some_and(|total| returned == limit as usize && (returned as u64) < total)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(items: Vec<u8>, total: Option<u64>) -> Page<u8> {
        Page {
            items,
            next: None,
            prev: None,
            last: None,
            first: None,
            total,
        }
    }

    #[test]
    fn truncation_footer_conditions_are_narrow() {
        let truncated = page(vec![1, 2, 3], Some(5));
        assert!(truncation_footer_needed(&truncated, 3));
        assert!(!truncation_footer_needed(&truncated, 4));
        assert!(!truncation_footer_needed(&page(vec![1, 2, 3], Some(3)), 3));
        assert!(!truncation_footer_needed(&page(vec![1, 2, 3], None), 3));
    }

    #[test]
    fn json_value_wraps_items_with_total_count() {
        let value = json_value(&page(vec![1, 2], Some(9)));
        assert_eq!(value["total_count"], 9);
        assert_eq!(value["items"], serde_json::json!([1, 2]));
    }
}
