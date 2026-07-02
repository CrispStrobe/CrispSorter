//! Offline synonym expansion for FTS queries (P24.4).
//!
//! Expands bare terms in a query string with OR-grouped synonyms before
//! the query is sent to the Tantivy FTS engine.  Supports both English
//! and German.  The synonym data is embedded at compile time — no
//! external file dependencies.
//!
//! Only bare alphabetic words ≥ 3 chars are expanded.  Operators, phrases,
//! wildcards, fuzzy markers, and numbers are left untouched.

use std::collections::HashMap;
use std::sync::LazyLock;

/// Build the synonym map at first access (compact: ~200 synonym groups).
static SYNONYMS: LazyLock<HashMap<&'static str, &'static [&'static str]>> = LazyLock::new(|| {
    let mut m = HashMap::new();
    // English synonym groups — each word maps to the full group.
    for group in EN_GROUPS {
        for &word in *group {
            m.insert(word, *group);
        }
    }
    // German synonym groups
    for group in DE_GROUPS {
        for &word in *group {
            m.insert(word, *group);
        }
    }
    m
});

/// Expand a query string: each bare word that has synonyms becomes
/// `(word OR syn1 OR syn2)`.  Returns the expanded query.
pub fn synonym_expand_query(query: &str) -> String {
    let mut result = Vec::new();
    let mut in_quote = false;
    for token in query.split_whitespace() {
        if token.starts_with('"') { in_quote = true; }
        if in_quote {
            result.push(token.to_string());
            if token.ends_with('"') && token.len() > 1 { in_quote = false; }
            continue;
        }
        // Skip operators and special syntax
        let upper = token.to_uppercase();
        if upper == "AND" || upper == "OR" || upper == "NOT"
            || token.contains("w/") || token.contains("pre/")
            || token.contains('~') || token.contains('*') || token.contains('?')
            || token.starts_with('(') || token.ends_with(')')
            || token.contains(':')
        {
            result.push(token.to_string());
            continue;
        }
        // Only expand alphabetic words ≥ 3 chars
        let lower = token.to_lowercase();
        if lower.len() >= 3
            && lower.chars().all(|c| c.is_alphabetic())
            && !lower.chars().all(|c| c.is_ascii_digit())
        {
            if let Some(group) = SYNONYMS.get(lower.as_str()) {
                let others: Vec<&str> = group.iter().copied().filter(|&s| s != lower.as_str()).collect();
                if !others.is_empty() {
                    let expanded = format!("({} OR {})", token, others.join(" OR "));
                    result.push(expanded);
                    continue;
                }
            }
        }
        result.push(token.to_string());
    }
    result.join(" ")
}

// ── English synonym groups ────────────────────────────────────────────
// Curated compact set — high-value synonyms for document search.

const EN_GROUPS: &[&[&str]] = &[
    &["begin", "start", "commence", "initiate"],
    &["end", "finish", "conclude", "terminate"],
    &["help", "assist", "aid", "support"],
    &["show", "display", "present", "exhibit"],
    &["buy", "purchase", "acquire", "obtain"],
    &["sell", "market", "vend"],
    &["big", "large", "huge", "enormous"],
    &["small", "tiny", "little", "minor"],
    &["fast", "quick", "rapid", "swift"],
    &["slow", "gradual", "unhurried"],
    &["important", "significant", "crucial", "vital"],
    &["problem", "issue", "challenge", "difficulty"],
    &["answer", "reply", "response"],
    &["ask", "question", "inquire", "query"],
    &["change", "modify", "alter", "adjust"],
    &["create", "make", "build", "construct"],
    &["destroy", "demolish", "eliminate", "remove"],
    &["increase", "grow", "rise", "expand"],
    &["decrease", "reduce", "decline", "shrink"],
    &["agree", "consent", "approve", "accept"],
    &["refuse", "reject", "decline", "deny"],
    &["allow", "permit", "enable", "authorize"],
    &["forbid", "prohibit", "ban", "prevent"],
    &["use", "utilize", "employ", "apply"],
    &["choose", "select", "pick", "opt"],
    &["error", "mistake", "fault", "defect"],
    &["idea", "concept", "notion", "thought"],
    &["plan", "strategy", "approach", "method"],
    &["result", "outcome", "consequence", "effect"],
    &["goal", "objective", "target", "aim"],
    &["part", "component", "element", "piece"],
    &["group", "team", "cluster", "collection"],
    &["money", "funds", "capital", "finance"],
    &["company", "firm", "corporation", "enterprise"],
    &["worker", "employee", "staff", "personnel"],
    &["customer", "client", "buyer", "consumer"],
    &["law", "regulation", "rule", "statute"],
    &["document", "file", "record", "paper"],
    &["report", "summary", "overview", "review"],
    &["meeting", "conference", "session", "gathering"],
    &["contract", "agreement", "deal", "arrangement"],
    &["price", "cost", "rate", "charge"],
    &["country", "nation", "state", "land"],
    &["area", "region", "zone", "district"],
    &["research", "study", "investigation", "analysis"],
    &["education", "training", "learning", "instruction"],
    &["health", "wellness", "wellbeing"],
    &["disease", "illness", "condition", "disorder"],
    &["environment", "nature", "ecology", "habitat"],
];

// ── German synonym groups ─────────────────────────────────────────────

const DE_GROUPS: &[&[&str]] = &[
    &["anfangen", "beginnen", "starten"],
    &["beenden", "aufhören", "abschließen"],
    &["helfen", "unterstützen", "beistehen"],
    &["zeigen", "darstellen", "vorführen", "anzeigen"],
    &["kaufen", "erwerben", "beschaffen"],
    &["verkaufen", "veräußern", "vertreiben"],
    &["groß", "riesig", "gewaltig", "umfangreich"],
    &["klein", "winzig", "gering", "minimal"],
    &["schnell", "rasch", "zügig", "flink"],
    &["langsam", "gemächlich", "träge"],
    &["wichtig", "bedeutend", "wesentlich", "entscheidend"],
    &["problem", "schwierigkeit", "herausforderung"],
    &["frage", "anfrage", "erkundigung"],
    &["antwort", "erwiderung", "reaktion"],
    &["ändern", "verändern", "modifizieren", "anpassen"],
    &["erstellen", "erzeugen", "herstellen", "schaffen"],
    &["löschen", "entfernen", "beseitigen"],
    &["erhöhen", "steigern", "vergrößern"],
    &["verringern", "reduzieren", "senken", "mindern"],
    &["erlauben", "gestatten", "genehmigen", "zulassen"],
    &["verbieten", "untersagen", "verhindern"],
    &["benutzen", "verwenden", "nutzen", "gebrauchen"],
    &["wählen", "auswählen", "aussuchen"],
    &["fehler", "irrtum", "mangel", "defekt"],
    &["idee", "gedanke", "konzept", "vorstellung"],
    &["plan", "strategie", "vorgehen", "methode"],
    &["ergebnis", "resultat", "folge", "auswirkung"],
    &["ziel", "zweck", "absicht"],
    &["teil", "bestandteil", "element", "stück"],
    &["gruppe", "team", "verband", "sammlung"],
    &["geld", "kapital", "mittel", "finanzen"],
    &["firma", "unternehmen", "betrieb", "konzern"],
    &["mitarbeiter", "angestellter", "beschäftigter"],
    &["kunde", "klient", "auftraggeber", "abnehmer"],
    &["gesetz", "verordnung", "regelung", "vorschrift"],
    &["dokument", "datei", "akte", "unterlage"],
    &["bericht", "zusammenfassung", "übersicht"],
    &["vertrag", "vereinbarung", "abkommen"],
    &["preis", "kosten", "gebühr", "tarif"],
    &["forschung", "studie", "untersuchung", "analyse"],
    &["bildung", "ausbildung", "schulung", "erziehung"],
    &["gesundheit", "wohlbefinden"],
    &["krankheit", "erkrankung", "leiden"],
    &["umwelt", "natur", "ökologie"],
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_known_synonym() {
        let q = synonym_expand_query("help with error");
        assert!(q.contains("assist"));
        assert!(q.contains("mistake"));
    }

    #[test]
    fn preserves_phrases() {
        let q = synonym_expand_query("\"help me\" fast");
        assert!(q.contains("\"help me\""));
        // "fast" should expand
        assert!(q.contains("quick") || q.contains("rapid"));
    }

    #[test]
    fn preserves_operators() {
        let q = synonym_expand_query("help AND fast");
        assert!(q.contains("AND"));
    }

    #[test]
    fn unknown_word_unchanged() {
        let q = synonym_expand_query("xyzzy");
        assert_eq!(q, "xyzzy");
    }

    #[test]
    fn german_synonym() {
        let q = synonym_expand_query("wichtig");
        assert!(q.contains("bedeutend"));
    }

    #[test]
    fn short_word_skipped() {
        let q = synonym_expand_query("do it");
        assert_eq!(q, "do it");
    }
}
