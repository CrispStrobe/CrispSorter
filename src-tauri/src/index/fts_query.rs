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
    IndexReader,
    query::{
        BooleanQuery, Occur,
        FuzzyTermQuery, PhraseQuery,
        TermQuery, Query,
        RegexQuery,
    },
    schema::{Field, IndexRecordOption},
    Term,
};
use anyhow::{Result, bail, anyhow};
use deunicode::deunicode;

/// Fields that the translator can target.
pub struct SearchFields {
    pub title: Field,
    pub body: Field,
    pub headings: Field,
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
    Word(String),       // bare term (may contain * ? ~ modifiers)
    Phrase(String),     // "quoted phrase"
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
                match word.to_uppercase().as_str() {
                    "AND" => tokens.push(Token::And),
                    "OR"  => tokens.push(Token::Or),
                    "NOT" => tokens.push(Token::Not),
                    upper if upper.starts_with("W/") => {
                        let n = word[2..].parse::<u32>()
                            .map_err(|_| anyhow!("Invalid w/N operator: {}", word))?;
                        tokens.push(Token::Within(n, false));
                    }
                    upper if upper.starts_with("PRE/") => {
                        let n = word[4..].parse::<u32>()
                            .map_err(|_| anyhow!("Invalid pre/N operator: {}", word))?;
                        tokens.push(Token::Within(n, true));
                    }
                    _ => tokens.push(Token::Word(word)),
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
        Parser { tokens, pos: 0, fields }
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
            } else if matches!(self.peek(), Token::Word(_) | Token::Phrase(_) | Token::LParen | Token::Not) {
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
            Ok(Box::new(BooleanQuery::new(vec![
                (Occur::MustNot, operand),
            ])))
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
                if matches!(self.peek(), Token::RParen) { self.consume(); }
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
/// - `foo*` / `fo?` → RegexQuery (wildcard)
/// - `foo~N`        → FuzzyTermQuery
/// - `foo`          → TermQuery on body + headings + title (weighted via BoostQuery)
fn build_term_query(word: &str, fields: &SearchFields) -> Result<Box<dyn Query>> {
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
    let head_q  = build_boosted_term(fields.headings, &folded, 2.0);
    let body_q  = build_boosted_term(fields.body, &folded, 1.0);

    Ok(Box::new(BooleanQuery::new(vec![
        (Occur::Should, title_q),
        (Occur::Should, head_q),
        (Occur::Should, body_q),
    ])))
}

fn build_boosted_term(field: Field, text: &str, boost: f32) -> Box<dyn Query> {
    use tantivy::query::BoostQuery;
    let tq = Box::new(TermQuery::new(
        Term::from_field_text(field, text),
        IndexRecordOption::WithFreqsAndPositions,
    ));
    if (boost - 1.0).abs() < 0.001 { tq } else { Box::new(BoostQuery::new(tq, boost)) }
}

/// Convert a wildcard pattern to a regex string.
fn wildcard_to_regex(pattern: &str) -> String {
    let mut out = String::with_capacity(pattern.len() * 2);
    for c in pattern.chars() {
        match c {
            '*'  => out.push_str(".*"),
            '?'  => out.push('.'),
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
    // SECURITY/PERF: Block leading wildcards if they are too expensive.
    if pattern.starts_with('*') || pattern.starts_with('?') {
        bail!("Leading wildcards ('{}') are not allowed for performance reasons.", pattern);
    }

    let regex = wildcard_to_regex(pattern);

    let title_q = build_boosted_regex(fields.title, &regex, 3.0)?;
    let head_q  = build_boosted_regex(fields.headings, &regex, 2.0)?;
    let body_q  = build_boosted_regex(fields.body, &regex, 1.0)?;

    Ok(Box::new(BooleanQuery::new(vec![
        (Occur::Should, title_q),
        (Occur::Should, head_q),
        (Occur::Should, body_q),
    ])))
}

fn build_boosted_regex(field: Field, regex: &str, boost: f32) -> Result<Box<dyn Query>> {
    use tantivy::query::BoostQuery;
    let rq = Box::new(RegexQuery::from_pattern(regex, field)
        .map_err(|e| anyhow!("Wildcard regex error: {:?}", e))?);

    if (boost - 1.0).abs() < 0.001 { Ok(rq) } else { Ok(Box::new(BoostQuery::new(rq, boost))) }
}

fn build_fuzzy(term: &str, distance: u8, fields: &SearchFields) -> Result<Box<dyn Query>> {
    use tantivy::query::BoostQuery;
    let title_q = Box::new(BoostQuery::new(Box::new(FuzzyTermQuery::new(Term::from_field_text(fields.title, term), distance, true)), 3.0));
    let head_q  = Box::new(BoostQuery::new(Box::new(FuzzyTermQuery::new(Term::from_field_text(fields.headings, term), distance, true)), 2.0));
    let body_q  = Box::new(FuzzyTermQuery::new(Term::from_field_text(fields.body, term), distance, true));

    Ok(Box::new(BooleanQuery::new(vec![
        (Occur::Should, title_q),
        (Occur::Should, head_q),
        (Occur::Should, body_q),
    ])))
}

/// Build an exact or slop phrase query on the body field.
fn build_phrase_query(text: &str, slop: u32, fields: &SearchFields) -> Result<Box<dyn Query>> {
    let tokens = simple_tokenize(text);
    if tokens.is_empty() { bail!("Empty phrase query"); }

    // For phrases, we also try matching title/headings, but with lower slop or higher boost.
    let title_q = build_boosted_phrase(fields.title, &tokens, slop, 3.0);
    let head_q  = build_boosted_phrase(fields.headings, &tokens, slop, 2.0);
    let body_q  = build_boosted_phrase(fields.body, &tokens, slop, 1.0);

    Ok(Box::new(BooleanQuery::new(vec![
        (Occur::Should, title_q),
        (Occur::Should, head_q),
        (Occur::Should, body_q),
    ])))
}

fn build_boosted_phrase(field: Field, tokens: &[String], slop: u32, boost: f32) -> Box<dyn Query> {
    use tantivy::query::BoostQuery;
    let terms: Vec<Term> = tokens.iter().map(|t| Term::from_field_text(field, t)).collect();

    let pq: Box<dyn Query> = if terms.len() == 1 {
        Box::new(TermQuery::new(terms[0].clone(), IndexRecordOption::WithFreqsAndPositions))
    } else {
        let phrase_terms: Vec<(usize, Term)> = terms.into_iter().enumerate().collect();
        let mut pq = PhraseQuery::new_with_offset(phrase_terms);
        pq.set_slop(slop);
        Box::new(pq)
    };

    if (boost - 1.0).abs() < 0.001 { pq } else { Box::new(BoostQuery::new(pq, boost)) }
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
            let mut fwd = PhraseQuery::new_with_offset(vec![
                (0usize, lt.clone()),
                (1usize, rt.clone()),
            ]);
            fwd.set_slop(slop);
            clauses.push((Occur::Should, Box::new(fwd)));

            if !ordered {
                // Backward: rt … lt  (bidirectional w/N)
                let mut bwd = PhraseQuery::new_with_offset(vec![
                    (0usize, rt.clone()),
                    (1usize, lt.clone()),
                ]);
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
        if t.field() == field { out.push(t.clone()); }
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
        .map(|t| fold_accents(t).trim_matches(|c: char| !c.is_alphanumeric()).to_owned())
        .filter(|t| !t.is_empty())
        .collect()
}

/// Normalise text to ASCII-ish (lowercase + accent folding).
pub fn fold_accents(text: &str) -> String {
    deunicode(text).to_lowercase()
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
        assert!(tokens.iter().any(|t| matches!(t, Token::Phrase(p) if p == "heiliger geist")));
    }

    #[test]
    fn lex_fuzzy() {
        let tokens = lex("gnade~2").unwrap();
        assert!(tokens.iter().any(|t| matches!(t, Token::Word(w) if w.contains('~'))));
    }

    #[test]
    fn lex_wildcard() {
        let tokens = lex("anon*").unwrap();
        assert!(tokens.iter().any(|t| matches!(t, Token::Word(w) if w.contains('*'))));
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
        use tempfile::TempDir;
        use crate::index::fts_index::FtsIndex;
        let dir = TempDir::new().unwrap();
        let idx = FtsIndex::open_or_create(dir.path()).unwrap();
        let mut w = idx.writer().unwrap();
        idx.add_document(&mut w, "d1", "u1", "en", "", "", "Karl Barth schrieb über Gnade").unwrap();
        idx.add_document(&mut w, "d2", "u1", "en", "", "", "Karl Marx schrieb über Kapital").unwrap();
        w.commit().unwrap();

        use crate::index::schema::SearchFilters;
        let hits = idx.search("Karl Barth", &SearchFilters::default(), 10).unwrap();
        // d1 contains both Karl AND Barth; d2 only has Karl — implicit AND must exclude d2
        assert_eq!(hits.len(), 1, "implicit AND should require both terms");
        assert_eq!(hits[0].doc_id, "d1");
    }

    #[test]
    fn wildcard_leading_blocked() {
        let fields = SearchFields {
            title: tantivy::schema::Field::from_field_id(0),
            headings: tantivy::schema::Field::from_field_id(1),
            body: tantivy::schema::Field::from_field_id(2),
        };
        let res = build_wildcard("*foo", &fields);
        assert!(res.is_err(), "Leading wildcard * should be blocked");
        let res2 = build_wildcard("?foo", &fields);
        assert!(res2.is_err(), "Leading wildcard ? should be blocked");
    }
}
