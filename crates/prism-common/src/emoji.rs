use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EmojiItem {
    pub emoji: String,
    pub name: String,
    pub category: String,
    #[serde(rename = "nameLc")]
    pub name_lc: String,
    #[serde(rename = "keywordsLc", default)]
    pub keywords_lc: Vec<String>,
    #[serde(rename = "aliasesLc", default)]
    pub aliases_lc: Vec<String>,
}

static EMBEDDED_EMOJIS_JSON: &str = include_str!("../../../data/emojis.json");

/// Load all 1,906 emoji items embedded at compile time
pub fn load_emojis() -> Vec<EmojiItem> {
    if let Ok(items) = serde_json::from_str::<Vec<EmojiItem>>(EMBEDDED_EMOJIS_JSON) {
        if !items.is_empty() {
            return items;
        }
    }
    fallback_emojis()
}

fn fallback_emojis() -> Vec<EmojiItem> {
    vec![
        EmojiItem {
            emoji: "😀".to_string(),
            name: "Grinning face".to_string(),
            category: "Smileys & Emotion".to_string(),
            name_lc: "grinning face".to_string(),
            keywords_lc: vec!["happy".to_string(), "smile".to_string()],
            aliases_lc: vec!["smile".to_string()],
        },
        EmojiItem {
            emoji: "🔥".to_string(),
            name: "Fire".to_string(),
            category: "Travel & Places".to_string(),
            name_lc: "fire".to_string(),
            keywords_lc: vec!["flame".to_string(), "hot".to_string(), "lit".to_string()],
            aliases_lc: vec!["fire".to_string(), "lit".to_string()],
        },
        EmojiItem {
            emoji: "🚀".to_string(),
            name: "Rocket".to_string(),
            category: "Travel & Places".to_string(),
            name_lc: "rocket".to_string(),
            keywords_lc: vec!["space".to_string(), "launch".to_string(), "fast".to_string()],
            aliases_lc: vec!["rocket".to_string()],
        },
        EmojiItem {
            emoji: "✨".to_string(),
            name: "Sparkles".to_string(),
            category: "Activities".to_string(),
            name_lc: "sparkles".to_string(),
            keywords_lc: vec!["stars".to_string(), "shine".to_string(), "magic".to_string()],
            aliases_lc: vec!["sparkles".to_string()],
        },
    ]
}

/// Search emoji items by query text and optional category, ranking by relevance
pub fn search_emojis(
    emojis: &[EmojiItem],
    query: &str,
    category: Option<&str>,
    limit: usize,
) -> Vec<EmojiItem> {
    let q = query.trim().to_lowercase();

    // Filter by category first if specified
    let candidate_iter = emojis.iter().filter(|item| {
        if let Some(cat) = category {
            if !cat.is_empty() && cat != "All" {
                return item.category.eq_ignore_ascii_case(cat);
            }
        }
        true
    });

    if q.is_empty() {
        return candidate_iter.take(limit).cloned().collect();
    }

    let mut scored: Vec<(i32, &EmojiItem)> = Vec::new();

    for item in candidate_iter {
        let mut score = 0;

        // Exact matches
        if item.name_lc == q {
            score += 100;
        } else if item.aliases_lc.iter().any(|a| a == &q) {
            score += 90;
        } else if item.name_lc.starts_with(&q) {
            score += 80;
        } else if item.aliases_lc.iter().any(|a| a.starts_with(&q)) {
            score += 70;
        } else if item.name_lc.split_whitespace().any(|w| w.starts_with(&q)) {
            score += 60;
        } else if item.keywords_lc.iter().any(|k| k == &q) {
            score += 50;
        } else if item.keywords_lc.iter().any(|k| k.starts_with(&q)) {
            score += 40;
        } else if item.name_lc.contains(&q) {
            score += 30;
        }

        if score > 0 {
            scored.push((score, item));
        }
    }

    scored.sort_by(|a, b| b.0.cmp(&a.0));
    scored.into_iter().take(limit).map(|(_, item)| item.clone()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_all_emojis() {
        let emojis = load_emojis();
        assert_eq!(emojis.len(), 1906);
    }

    #[test]
    fn test_search_emojis_scoring() {
        let emojis = load_emojis();
        let results = search_emojis(&emojis, "fire", None, 5);
        assert!(!results.is_empty());
        assert_eq!(results[0].emoji, "🔥");

        let food_results = search_emojis(&emojis, "", Some("Food & Drink"), 10);
        assert!(!food_results.is_empty());
        assert!(food_results.iter().all(|e| e.category == "Food & Drink"));
    }
}
