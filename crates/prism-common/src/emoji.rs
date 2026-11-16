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

fn boundary(c: char) -> bool {
    c == ' ' || c == '-' || c == '_'
}

/// QML Fuzzy.scoreTiered: exact > whole word > prefix > word-start substring > substring >
/// subsequence (tokens of at least `min_subseq` chars). None when nothing matches.
fn score_tiered(token: &str, text: &str, min_subseq: usize) -> Option<f64> {
    if text.is_empty() || token.is_empty() {
        return None;
    }
    let t: Vec<char> = text.chars().collect();
    let k: Vec<char> = token.chars().collect();
    let (sl, tl) = (t.len() as f64, k.len());
    if text == token {
        return Some(120.0);
    }
    let starts: Vec<usize> = (0..t.len()).filter(|&i| t[i..].starts_with(&k[..])).collect();
    for &i in &starts {
        let before = i == 0 || boundary(t[i - 1]);
        let after = i + tl == t.len() || boundary(t[i + tl]);
        if before && after {
            return Some(90.0 - sl * 0.05);
        }
    }
    if starts.first() == Some(&0) {
        return Some(70.0 - sl * 0.05);
    }
    if starts.iter().any(|&i| i > 0 && boundary(t[i - 1])) {
        return Some(60.0 - sl * 0.05);
    }
    if let Some(&i) = starts.first() {
        return Some(40.0 - i as f64 * 0.5 - sl * 0.05);
    }
    if tl >= min_subseq {
        let (mut qi, mut score, mut last) = (0usize, 0.0, -2isize);
        for (ti, &c) in t.iter().enumerate() {
            if qi == tl {
                break;
            }
            if c == k[qi] {
                score += if last == ti as isize - 1 { 3.0 } else { 1.0 };
                if ti == 0 || boundary(t[ti - 1]) {
                    score += 4.0;
                }
                last = ti as isize;
                qi += 1;
            }
        }
        if qi == tl {
            return Some(15.0 + score - sl * 0.1);
        }
    }
    None
}

/// QML Fuzzy.scoreEmoji: the glyph itself scores 1000; otherwise every query word must match
/// the name, an alias (x0.9), a keyword (x0.7) or the category (x0.4); scores add up.
pub fn score_emoji(query: &str, item: &EmojiItem) -> Option<f64> {
    let q = query.trim();
    if q.is_empty() {
        return None;
    }
    if item.emoji.replace('\u{FE0F}', "") == q.replace('\u{FE0F}', "") {
        return Some(1000.0);
    }
    let category = item.category.to_lowercase();
    let mut total = 0.0;
    for token in q.to_lowercase().split_whitespace() {
        let mut best = score_tiered(token, &item.name_lc, 4);
        let mut take = |s: Option<f64>, w: f64| {
            if let Some(s) = s {
                if best.map(|b| s * w > b).unwrap_or(true) {
                    best = Some(s * w);
                }
            }
        };
        for a in &item.aliases_lc {
            take(score_tiered(token, a, 4), 0.9);
        }
        for k in &item.keywords_lc {
            take(score_tiered(token, k, 4), 0.7);
        }
        take(score_tiered(token, &category, 4), 0.4);
        total += best?;
    }
    Some(total)
}

/// Emojis matching `query` best first (or, with an empty query, all of `category`), at most
/// `limit`.
pub fn search_emojis(emojis: &[EmojiItem], query: &str, category: Option<&str>, limit: usize) -> Vec<EmojiItem> {
    let in_cat = |item: &&EmojiItem| match category {
        Some(c) if !c.is_empty() && c != "All" => item.category.eq_ignore_ascii_case(c),
        _ => true,
    };
    if query.trim().is_empty() {
        return emojis.iter().filter(in_cat).take(limit).cloned().collect();
    }
    let mut scored: Vec<(f64, &EmojiItem)> = emojis.iter().filter(in_cat).filter_map(|e| score_emoji(query, e).filter(|s| *s > 0.0).map(|s| (s, e))).collect();
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    scored.into_iter().take(limit).map(|(_, e)| e.clone()).collect()
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
