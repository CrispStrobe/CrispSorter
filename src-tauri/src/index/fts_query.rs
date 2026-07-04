use anyhow::{anyhow, bail, Result};
use deunicode::deunicode;
/// Boolean query translator → Tantivy query tree.
///
/// Supported syntax:
///
///   foo AND bar          boolean must
///   foo OR bar           boolean should
///   NOT foo              boolean must_not
///   "foo bar"            exact phrase (slop 0)
///   foo*                 prefix wildcard  (expanded via RegexQuery on the TermDictionary)
///   fo?                  single-char wildcard (one arbitrary character)
///   foo~2                fuzzy, edit distance 2
///   foo w/N bar          within N words, either order  (bidirectional slop)
///   foo pre/N bar        foo before bar within N words (ordered slop)
///   (foo OR bar) w/N baz grouped proximity (cross-product slop)
///   foo bar              implicit AND (adjacent terms require both)
///
/// Wildcard expansion (`foo*`, `fo?`) converts the pattern to a regex using the
/// same rules as Tantivy's own wildcard helpers: `*` → `.*`, `?` → `.`, other
/// regex metacharacters are escaped.  tantivy-fst applies implicit full-match
/// anchoring (`^…$`) when scanning the TermDictionary, so no explicit anchors
/// are added here (they are not supported by the FST regex engine).
use tantivy::{
    query::{BooleanQuery, FuzzyTermQuery, Occur, PhraseQuery, Query, RegexQuery, TermQuery},
    schema::{Field, IndexRecordOption},
    IndexReader, Term,
};

/// Fields that the translator can target.
pub struct SearchFields {
    pub title: Field,
    pub body: Field,
    pub headings: Field,
    /// `Some` iff the on-disk Tantivy schema has the `body_translated`
    /// field — added in the FTS-over-translated-body P13.5 follow-up.
    /// When `Some`, every bare-term query OR's a low-boost match against
    /// the translated body so an English query against a Bosnian doc
    /// with an English translation still scores via BM25.  When `None`
    /// (legacy index), the translator silently skips it.
    pub body_translated: Option<Field>,
}

/// Translate a query string into a Tantivy `Box<dyn Query>`.
pub fn translate(
    query_str: &str,
    _reader: &IndexReader,
    fields: &SearchFields,
) -> Result<Box<dyn Query>> {
    let tokens = lex(query_str)?;
    let mut parser = Parser::new(tokens, fields);
    parser.parse_expr()
}

// ── Lexer ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Word(String),   // bare term (may contain * ? ~ modifiers)
    Phrase(String), // "quoted phrase"
    And,
    Or,
    Not,
    Within(u32, bool), // w/N (false) or pre/N (true = ordered)
    LParen,
    RParen,
    Eof,
}

fn lex(input: &str) -> Result<Vec<Token>> {
    let mut tokens = Vec::new();
    let mut chars = input.char_indices().peekable();

    while let Some((_i, c)) = chars.next() {
        match c {
            ' ' | '\t' | '\n' => {}
            '(' => tokens.push(Token::LParen),
            ')' => tokens.push(Token::RParen),
            '"' => {
                let mut phrase = String::new();
                loop {
                    match chars.next() {
                        Some((_, '"')) | None => break,
                        Some((_, ch)) => phrase.push(ch),
                    }
                }
                tokens.push(Token::Phrase(phrase));
            }
            _ => {
                let mut word = String::from(c);
                while let Some(&(_, nc)) = chars.peek() {
                    if nc == ' ' || nc == '\t' || nc == '\n' || nc == '(' || nc == ')' {
                        break;
                    }
                    chars.next();
                    word.push(nc);
                }
                if word.eq_ignore_ascii_case("AND") {
                    tokens.push(Token::And);
                } else if word.eq_ignore_ascii_case("OR") {
                    tokens.push(Token::Or);
                } else if word.eq_ignore_ascii_case("NOT") {
                    tokens.push(Token::Not);
                } else if word.is_ascii()
                    && word.len() > 2
                    && word[..2].eq_ignore_ascii_case("W/")
                {
                    let n = word[2..]
                        .parse::<u32>()
                        .map_err(|_| anyhow!("Invalid w/N operator: {}", word))?;
                    tokens.push(Token::Within(n, false));
                } else if word.is_ascii()
                    && word.len() > 4
                    && word[..4].eq_ignore_ascii_case("PRE/")
                {
                    let n = word[4..]
                        .parse::<u32>()
                        .map_err(|_| anyhow!("Invalid pre/N operator: {}", word))?;
                    tokens.push(Token::Within(n, true));
                } else {
                    tokens.push(Token::Word(word));
                }
            }
        }
    }
    tokens.push(Token::Eof);
    Ok(tokens)
}

// ── Recursive-descent parser ───────────────────────────────────────────────

struct Parser<'a> {
    tokens: Vec<Token>,
    pos: usize,
    fields: &'a SearchFields,
}

impl<'a> Parser<'a> {
    fn new(tokens: Vec<Token>, fields: &'a SearchFields) -> Self {
        Parser {
            tokens,
            pos: 0,
            fields,
        }
    }

    fn peek(&self) -> &Token {
        self.tokens.get(self.pos).unwrap_or(&Token::Eof)
    }

    fn consume(&mut self) -> Token {
        let t = self.tokens.get(self.pos).cloned().unwrap_or(Token::Eof);
        self.pos += 1;
        t
    }

    /// Entry: parse a boolean OR expression (lowest precedence).
    fn parse_expr(&mut self) -> Result<Box<dyn Query>> {
        let mut lhs = self.parse_and()?;
        while matches!(self.peek(), Token::Or) {
            self.consume();
            let rhs = self.parse_and()?;
            lhs = Box::new(BooleanQuery::new(vec![
                (Occur::Should, lhs),
                (Occur::Should, rhs),
            ]));
        }
        Ok(lhs)
    }

    fn parse_and(&mut self) -> Result<Box<dyn Query>> {
        let mut lhs = self.parse_not()?;
        loop {
            if matches!(self.peek(), Token::And) {
                self.consume();
                let rhs = self.parse_not()?;
                lhs = Box::new(BooleanQuery::new(vec![
                    (Occur::Must, lhs),
                    (Occur::Must, rhs),
                ]));
            } else if matches!(
                self.peek(),
                Token::Word(_) | Token::Phrase(_) | Token::LParen | Token::Not
            ) {
                // Implicit AND: adjacent terms without an explicit operator
                let rhs = self.parse_not()?;
                lhs = Box::new(BooleanQuery::new(vec![
                    (Occur::Must, lhs),
                    (Occur::Must, rhs),
                ]));
            } else {
                break;
            }
        }
        Ok(lhs)
    }

    fn parse_not(&mut self) -> Result<Box<dyn Query>> {
        if matches!(self.peek(), Token::Not) {
            self.consume();
            let operand = self.parse_proximity()?;
            Ok(Box::new(BooleanQuery::new(vec![(Occur::MustNot, operand)])))
        } else {
            self.parse_proximity()
        }
    }

    /// Proximity: `atom w/N atom` or `atom pre/N atom`.
    fn parse_proximity(&mut self) -> Result<Box<dyn Query>> {
        let lhs = self.parse_atom()?;
        if let Token::Within(n, ordered) = self.peek().clone() {
            self.consume();
            let rhs = self.parse_atom()?;
            return build_proximity(lhs, rhs, n, ordered, self.fields);
        }
        Ok(lhs)
    }

    fn parse_atom(&mut self) -> Result<Box<dyn Query>> {
        match self.peek().clone() {
            Token::LParen => {
                self.consume();
                let inner = self.parse_expr()?;
                if matches!(self.peek(), Token::RParen) {
                    self.consume();
                }
                Ok(inner)
            }
            Token::Phrase(text) => {
                self.consume();
                build_phrase_query(&text, 0, self.fields)
            }
            Token::Word(word) => {
                self.consume();
                build_term_query(&word, self.fields)
            }
            Token::Eof => bail!("Unexpected end of query"),
            other => bail!("Unexpected token: {:?}", other),
        }
    }
}

// ── Query builders ─────────────────────────────────────────────────────────

/// Build a query for a single term word.
/// - `field:foo`    → TermQuery scoped to the named field (PLAN P7.2)
/// - `foo*` / `fo?` → RegexQuery (wildcard)
/// - `foo~N`        → FuzzyTermQuery
/// - `foo`          → TermQuery on body + headings + title (weighted via BoostQuery)
fn build_term_query(word: &str, fields: &SearchFields) -> Result<Box<dyn Query>> {
    // Field-prefix syntax: `title:foo`, `body:foo`, `headings:foo`.
    // PLAN P7.2 — restricts the match to a single field instead of the
    // default boosted union across all three. Phrases inside the value
    // aren't supported here (the lexer eats the `"` as a separate
    // Phrase token); use `title:* AND "phrase"` for that effect.
    if let Some(colon) = word.find(':') {
        let prefix = &word[..colon];
        let value = &word[colon + 1..];
        if !value.is_empty() {
            let field_opt = match prefix {
                "title" => Some(fields.title),
                "headings" | "h" => Some(fields.headings),
                "body" | "text" => Some(fields.body),
                _ => None,
            };
            if let Some(f) = field_opt {
                let folded = fold_accents(value);
                // Wildcards / fuzzy still apply inside a field-scoped term —
                // recurse through the regular term-query dispatch but with
                // a single-field SearchFields so the boosted union becomes
                // a single boosted Term on `f`.  body_translated is dropped
                // here on purpose: a field-prefixed query like `title:hello`
                // shouldn't also fan out into the translated body — that
                // would surprise the user.
                let scoped = SearchFields {
                    title: f,
                    headings: f,
                    body: f,
                    body_translated: None,
                };
                return build_term_query(&folded, &scoped);
            }
            // Unknown prefix → fall through to treat the whole `prefix:value`
            // as a literal term. Surprising but predictable; the alternative
            // (silently dropping the colon) hides bad queries.
        }
    }

    let folded = fold_accents(word);

    // Fuzzy: foo~N
    if let Some(tilde_pos) = folded.rfind('~') {
        let term_str = &folded[..tilde_pos];
        let dist: u8 = folded[tilde_pos + 1..].parse().unwrap_or(1);
        return build_fuzzy(term_str, dist, fields);
    }

    // Wildcard: foo* or fo?
    if folded.contains('*') || folded.contains('?') {
        return build_wildcard(&folded, fields);
    }

    // Exact term across fields with boosting.
    let title_q = build_boosted_term(fields.title, &folded, 3.0);
    let head_q = build_boosted_term(fields.headings, &folded, 2.0);
    let body_q = build_boosted_term(fields.body, &folded, 1.0);

    let mut clauses: Vec<(Occur, Box<dyn Query>)> = vec![
        (Occur::Should, title_q),
        (Occur::Should, head_q),
        (Occur::Should, body_q),
    ];

    // FTS-over-translated body: when the schema has body_translated,
    // OR-merge a slightly lower-boost match against it.  0.7 < 1.0 so
    // original-language hits outrank translated-only hits when both
    // fire, but translated-only hits still score above zero and so
    // appear in the BM25 channel of the hybrid search.  Wildcards /
    // fuzzy queries above already returned before reaching this point,
    // so body_translated only joins the exact-term path — matches the
    // user expectation for a translated-text channel (whole words).
    if let Some(bt) = fields.body_translated {
        clauses.push((Occur::Should, build_boosted_term(bt, &folded, 0.7)));
    }

    Ok(Box::new(BooleanQuery::new(clauses)))
}

fn build_boosted_term(field: Field, text: &str, boost: f32) -> Box<dyn Query> {
    use tantivy::query::BoostQuery;
    let tq = Box::new(TermQuery::new(
        Term::from_field_text(field, text),
        IndexRecordOption::WithFreqsAndPositions,
    ));
    if (boost - 1.0).abs() < 0.001 {
        tq
    } else {
        Box::new(BoostQuery::new(tq, boost))
    }
}

/// Convert a wildcard pattern to a regex string.
fn wildcard_to_regex(pattern: &str) -> String {
    let mut out = String::with_capacity(pattern.len() * 2);
    for c in pattern.chars() {
        match c {
            '*' => out.push_str(".*"),
            '?' => out.push('.'),
            '.' | '+' | '^' | '$' | '{' | '}' | '(' | ')' | '|' | '[' | ']' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
    }
    out
}

fn build_wildcard(pattern: &str, fields: &SearchFields) -> Result<Box<dyn Query>> {
    let regex = wildcard_to_regex(pattern);

    let title_q = build_boosted_regex(fields.title, &regex, 3.0)?;
    let head_q = build_boosted_regex(fields.headings, &regex, 2.0)?;
    let body_q = build_boosted_regex(fields.body, &regex, 1.0)?;

    Ok(Box::new(BooleanQuery::new(vec![
        (Occur::Should, title_q),
        (Occur::Should, head_q),
        (Occur::Should, body_q),
    ])))
}

fn build_boosted_regex(field: Field, regex: &str, boost: f32) -> Result<Box<dyn Query>> {
    use tantivy::query::BoostQuery;
    let rq = Box::new(
        RegexQuery::from_pattern(regex, field)
            .map_err(|e| anyhow!("Wildcard regex error: {:?}", e))?,
    );

    if (boost - 1.0).abs() < 0.001 {
        Ok(rq)
    } else {
        Ok(Box::new(BoostQuery::new(rq, boost)))
    }
}

fn build_fuzzy(term: &str, distance: u8, fields: &SearchFields) -> Result<Box<dyn Query>> {
    use tantivy::query::BoostQuery;
    let title_q = Box::new(BoostQuery::new(
        Box::new(FuzzyTermQuery::new(
            Term::from_field_text(fields.title, term),
            distance,
            true,
        )),
        3.0,
    ));
    let head_q = Box::new(BoostQuery::new(
        Box::new(FuzzyTermQuery::new(
            Term::from_field_text(fields.headings, term),
            distance,
            true,
        )),
        2.0,
    ));
    let body_q = Box::new(FuzzyTermQuery::new(
        Term::from_field_text(fields.body, term),
        distance,
        true,
    ));

    Ok(Box::new(BooleanQuery::new(vec![
        (Occur::Should, title_q),
        (Occur::Should, head_q),
        (Occur::Should, body_q),
    ])))
}

/// Build an exact or slop phrase query on the body field.
fn build_phrase_query(text: &str, slop: u32, fields: &SearchFields) -> Result<Box<dyn Query>> {
    let tokens = simple_tokenize(text);
    if tokens.is_empty() {
        bail!("Empty phrase query");
    }

    // For phrases, we also try matching title/headings, but with lower slop or higher boost.
    let title_q = build_boosted_phrase(fields.title, &tokens, slop, 3.0);
    let head_q = build_boosted_phrase(fields.headings, &tokens, slop, 2.0);
    let body_q = build_boosted_phrase(fields.body, &tokens, slop, 1.0);

    Ok(Box::new(BooleanQuery::new(vec![
        (Occur::Should, title_q),
        (Occur::Should, head_q),
        (Occur::Should, body_q),
    ])))
}

fn build_boosted_phrase(field: Field, tokens: &[String], slop: u32, boost: f32) -> Box<dyn Query> {
    use tantivy::query::BoostQuery;
    let terms: Vec<Term> = tokens
        .iter()
        .map(|t| Term::from_field_text(field, t))
        .collect();

    let pq: Box<dyn Query> = if terms.len() == 1 {
        Box::new(TermQuery::new(
            terms[0].clone(),
            IndexRecordOption::WithFreqsAndPositions,
        ))
    } else {
        let phrase_terms: Vec<(usize, Term)> = terms.into_iter().enumerate().collect();
        let mut pq = PhraseQuery::new_with_offset(phrase_terms);
        pq.set_slop(slop);
        Box::new(pq)
    };

    if (boost - 1.0).abs() < 0.001 {
        pq
    } else {
        Box::new(BoostQuery::new(pq, boost))
    }
}

/// Build a proximity query for `lhs w/N rhs` or `lhs pre/N rhs`.
///
/// Extracts leaf Terms from both sides and cross-products them into slop queries.
/// For `w/N` (bidirectional): emits both orderings as BooleanQuery::should.
/// For `pre/N` (ordered): emits only forward ordering.
fn build_proximity(
    lhs: Box<dyn Query>,
    rhs: Box<dyn Query>,
    slop: u32,
    ordered: bool,
    fields: &SearchFields,
) -> Result<Box<dyn Query>> {
    let lhs_terms = extract_body_terms(&*lhs, fields.body);
    let rhs_terms = extract_body_terms(&*rhs, fields.body);

    if lhs_terms.is_empty() || rhs_terms.is_empty() {
        // Fall back to AND if terms can't be extracted (e.g. nested complex queries)
        return Ok(Box::new(BooleanQuery::new(vec![
            (Occur::Must, lhs),
            (Occur::Must, rhs),
        ])));
    }

    let mut clauses: Vec<(Occur, Box<dyn Query>)> = Vec::new();

    for lt in &lhs_terms {
        for rt in &rhs_terms {
            // Forward: lt … rt
            let mut fwd =
                PhraseQuery::new_with_offset(vec![(0usize, lt.clone()), (1usize, rt.clone())]);
            fwd.set_slop(slop);
            clauses.push((Occur::Should, Box::new(fwd)));

            if !ordered {
                // Backward: rt … lt  (bidirectional w/N)
                let mut bwd =
                    PhraseQuery::new_with_offset(vec![(0usize, rt.clone()), (1usize, lt.clone())]);
                bwd.set_slop(slop);
                clauses.push((Occur::Should, Box::new(bwd)));
            }
        }
    }

    Ok(Box::new(BooleanQuery::new(clauses)))
}

/// Extract leaf Terms from a query, restricted to `field`.
fn extract_body_terms(query: &dyn Query, field: Field) -> Vec<Term> {
    let mut out = Vec::new();
    if let Some(tq) = query.as_any().downcast_ref::<TermQuery>() {
        let t = tq.term();
        if t.field() == field {
            out.push(t.clone());
        }
    } else if let Some(bq) = query.as_any().downcast_ref::<BooleanQuery>() {
        for (_, sub) in bq.clauses() {
            out.extend(extract_body_terms(sub.as_ref(), field));
        }
    }
    out
}

/// Naive whitespace + lowercase tokenizer for phrase query construction.
pub fn simple_tokenize(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|t| {
            fold_accents(t)
                .trim_matches(|c: char| !c.is_alphanumeric())
                .to_owned()
        })
        .filter(|t| !t.is_empty())
        .collect()
}

/// Normalise text to ASCII-ish (lowercase + accent folding).
pub fn fold_accents(text: &str) -> String {
    deunicode(text).to_lowercase()
}

/// Rewrite a query string to add fuzzy matching (~1) to every bare word.
/// Preserves phrases, operators, wildcards, and existing fuzzy markers.
/// Only words with 4+ chars are fuzzified (short words produce too many
/// false-positive matches at edit-distance 1).
pub fn fuzzify_query(query: &str) -> String {
    let mut result = Vec::new();
    let mut in_quote = false;
    for token in query.split_whitespace() {
        if token.starts_with('"') {
            in_quote = true;
        }
        if in_quote {
            result.push(token.to_string());
            if token.ends_with('"') && token.len() > 1 {
                in_quote = false;
            }
            continue;
        }
        // Skip operators (avoid to_uppercase() allocation)
        if token.eq_ignore_ascii_case("AND")
            || token.eq_ignore_ascii_case("OR")
            || token.eq_ignore_ascii_case("NOT")
            || token.contains("w/")
            || token.contains("pre/")
        {
            result.push(token.to_string());
            continue;
        }
        // Skip if already has fuzzy/wildcard markers
        if token.contains('~')
            || token.contains('*')
            || token.contains('?')
            || token.starts_with('(')
            || token.ends_with(')')
            || token.contains(':')
        {
            result.push(token.to_string());
            continue;
        }
        // Only fuzzify alphabetic words >= 4 chars.
        // Pure numbers (years, IDs) should match exactly.
        if token.len() >= 4 && !token.chars().all(|c| c.is_ascii_digit()) {
            result.push(format!("{}~1", token));
        } else {
            result.push(token.to_string());
        }
    }
    result.join(" ")
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lex_basic_boolean() {
        let tokens = lex("rahner AND NOT barth").unwrap();
        assert!(tokens.iter().any(|t| matches!(t, Token::And)));
        assert!(tokens.iter().any(|t| matches!(t, Token::Not)));
    }

    #[test]
    fn lex_within() {
        let tokens = lex("rahner w/50 anon*").unwrap();
        assert!(tokens.iter().any(|t| matches!(t, Token::Within(50, false))));
    }

    #[test]
    fn lex_pre() {
        let tokens = lex("god pre/10 grace").unwrap();
        assert!(tokens.iter().any(|t| matches!(t, Token::Within(10, true))));
    }

    #[test]
    fn lex_phrase() {
        let tokens = lex("\"heiliger geist\"").unwrap();
        assert!(tokens
            .iter()
            .any(|t| matches!(t, Token::Phrase(p) if p == "heiliger geist")));
    }

    #[test]
    fn lex_fuzzy() {
        let tokens = lex("gnade~2").unwrap();
        assert!(tokens
            .iter()
            .any(|t| matches!(t, Token::Word(w) if w.contains('~'))));
    }

    #[test]
    fn lex_wildcard() {
        let tokens = lex("anon*").unwrap();
        assert!(tokens
            .iter()
            .any(|t| matches!(t, Token::Word(w) if w.contains('*'))));
    }

    #[test]
    fn simple_tokenize_cleans() {
        let tokens = simple_tokenize("Heiliger Geist!");
        assert_eq!(tokens, vec!["heiliger", "geist"]);
    }

    #[test]
    fn lex_case_insensitive_operators() {
        let tokens = lex("foo or bar").unwrap();
        assert!(tokens.iter().any(|t| matches!(t, Token::Or)));
    }

    #[test]
    fn implicit_and_multi_word() {
        // "Karl Barth" should parse as Karl AND Barth (not just Karl)
        use crate::index::fts_index::{FtsIndex, TantivyInput};
        use tempfile::TempDir;
        let dir = TempDir::new().unwrap();
        let idx = FtsIndex::open_or_create(dir.path()).unwrap();
        let mut w = idx.writer().unwrap();
        idx.add_document(
            &mut w,
            TantivyInput {
                doc_id: "d1",
                owner_id: "u1",
                language: "en",
                title: "",
                headings: "",
                body: "Karl Barth schrieb über Gnade",
                body_translated: None,
            },
        )
        .unwrap();
        idx.add_document(
            &mut w,
            TantivyInput {
                doc_id: "d2",
                owner_id: "u1",
                language: "en",
                title: "",
                headings: "",
                body: "Karl Marx schrieb über Kapital",
                body_translated: None,
            },
        )
        .unwrap();
        w.commit().unwrap();

        use crate::index::schema::SearchFilters;
        let hits = idx
            .search("Karl Barth", &SearchFilters::default(), 10)
            .unwrap();
        // d1 contains both Karl AND Barth; d2 only has Karl — implicit AND must exclude d2
        assert_eq!(hits.len(), 1, "implicit AND should require both terms");
        assert_eq!(hits[0].doc_id, "d1");
    }

    #[test]
    fn wildcard_leading_allowed() {
        let fields = SearchFields {
            title: tantivy::schema::Field::from_field_id(0),
            headings: tantivy::schema::Field::from_field_id(1),
            body: tantivy::schema::Field::from_field_id(2),
            body_translated: None,
        };
        let res = build_wildcard("*foo", &fields);
        assert!(res.is_ok(), "Leading wildcard * should now be allowed");
        let res2 = build_wildcard("?foo", &fields);
        assert!(res2.is_ok(), "Leading wildcard ? should now be allowed");
    }

    /// PLAN P7.2 — `field:term` syntax restricts the match to a single
    /// indexed field instead of the default boosted union across all
    /// three. The body of the query is still wildcard / fuzzy aware
    /// because we recurse through `build_term_query` with a
    /// single-field SearchFields.
    #[test]
    fn field_prefix_scopes_to_named_field() {
        let title_f = tantivy::schema::Field::from_field_id(0);
        let head_f = tantivy::schema::Field::from_field_id(1);
        let body_f = tantivy::schema::Field::from_field_id(2);
        let fields = SearchFields {
            title: title_f,
            headings: head_f,
            body: body_f,
            body_translated: None,
        };

        // `title:karl` should produce a TermQuery on the title field only,
        // not the boosted three-way union the bare term `karl` would yield.
        let q = build_term_query("title:karl", &fields).unwrap();
        // The boosted union returns a BooleanQuery with 3 Should clauses;
        // a field-scoped term returns one with all 3 clauses pointing at
        // the same field (since the recursion still goes through the
        // boosted-union path with a single-field SearchFields).
        if let Some(bq) = q.as_any().downcast_ref::<BooleanQuery>() {
            for (_, sub) in bq.clauses() {
                // Every leaf TermQuery should be on the title field.
                let body_terms = extract_body_terms(sub.as_ref(), title_f);
                let head_terms = extract_body_terms(sub.as_ref(), head_f);
                let body_field_terms = extract_body_terms(sub.as_ref(), body_f);
                assert!(
                    !body_terms.is_empty()
                        || head_terms.is_empty() && body_field_terms.is_empty(),
                    "field-scoped term must only emit on the named field"
                );
            }
        } else {
            panic!("expected a BooleanQuery, got something else");
        }
    }

    /// Unknown prefixes (`foo:bar` for non-whitelisted `foo`) fall
    /// through to be treated as a literal term. Surprising but
    /// predictable: we'd rather a typo'd field name surface as
    /// "no results" than silently search the wrong field or throw an
    /// error mid-typing.
    #[test]
    fn field_prefix_unknown_falls_through() {
        let fields = SearchFields {
            title: tantivy::schema::Field::from_field_id(0),
            headings: tantivy::schema::Field::from_field_id(1),
            body: tantivy::schema::Field::from_field_id(2),
            body_translated: None,
        };
        let q = build_term_query("notafield:karl", &fields);
        assert!(q.is_ok(), "unknown prefix should still produce a query");
    }

    #[test]
    fn fuzzify_adds_tilde_to_bare_words() {
        assert_eq!(
            fuzzify_query("climate change 2024"),
            "climate~1 change~1 2024"
        );
        assert_eq!(
            fuzzify_query("\"exact phrase\" AND foo*"),
            "\"exact phrase\" AND foo*"
        );
        // "the" is 3 chars → not fuzzified; "long" is 4 → fuzzified
        assert_eq!(fuzzify_query("the long road"), "the long~1 road~1");
    }

    #[test]
    fn fuzzify_preserves_field_prefix() {
        // Tokens containing ':' must pass through unchanged to preserve
        // field-scoped syntax like "title:karl" or "body:foo".
        let out = fuzzify_query("title:karl body:foo");
        assert_eq!(out, "title:karl body:foo",
            "field-prefixed tokens must not be fuzzified: {out}");
    }

    #[test]
    fn fuzzify_skips_parentheses() {
        // Tokens that start with '(' or end with ')' must be left alone.
        let out = fuzzify_query("(climate OR weather)");
        assert_eq!(out, "(climate OR weather)",
            "parenthesized tokens must be preserved: {out}");
    }

    #[test]
    fn fuzzify_handles_empty() {
        // Empty input must return an empty string without panicking.
        assert_eq!(fuzzify_query(""), "", "empty input must produce empty output");
    }

    #[test]
    fn lex_mixed_case_within_pre() {
        // W/ and PRE/ must be recognised in any casing.
        let t1 = lex("a W/5 b").unwrap();
        assert!(t1.iter().any(|t| matches!(t, Token::Within(5, false))));
        let t2 = lex("a w/5 b").unwrap();
        assert!(t2.iter().any(|t| matches!(t, Token::Within(5, false))));
        let t3 = lex("a PRE/3 b").unwrap();
        assert!(t3.iter().any(|t| matches!(t, Token::Within(3, true))));
        let t4 = lex("a pre/3 b").unwrap();
        assert!(t4.iter().any(|t| matches!(t, Token::Within(3, true))));
    }

    #[test]
    fn lex_unicode_word_not_panics() {
        // A multi-byte word like "über" must not panic on the W/ / PRE/
        // byte-slice check — the `is_ascii()` guard protects it.
        let tokens = lex("über Müller").unwrap();
        assert!(tokens.iter().any(|t| matches!(t, Token::Word(w) if w == "über")));
        assert!(tokens.iter().any(|t| matches!(t, Token::Word(w) if w == "Müller")));
    }

    #[test]
    fn fuzzify_operators_case_insensitive() {
        // Operators in any casing must be preserved by fuzzify_query.
        for op in &["AND", "and", "And", "OR", "or", "NOT", "not"] {
            let input = format!("climate {} weather", op);
            let out = fuzzify_query(&input);
            assert!(out.contains(op), "operator {op} must survive fuzzify: {out}");
        }
    }
}
