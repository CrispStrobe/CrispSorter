//! Validated full-text query language and positional matching.
//!
//! AND binds more tightly than OR. Adjacent operands imply AND. NOT is a
//! complement, including at the root. Fields accept a term, a quoted phrase,
//! or a parenthesized expression. `w/N` allows either order and `pre/N` requires
//! the left operand before the right, with at most N intervening indexed words.
//! Proximity operands support words, exact phrases, and OR alternatives. An
//! unsupported shape is an error; it is never silently changed into an AND.
pub mod explanation;
pub mod query_plan;
pub mod synonyms;

use anyhow::{anyhow, bail, Result};
use std::collections::BTreeSet;
use tantivy::{
    postings::SegmentPostings,
    query::{
        AllQuery, BooleanQuery, BoostQuery, EmptyQuery, EnableScoring, Explanation, FuzzyTermQuery,
        Occur, PhraseQuery, Query, RegexQuery, Scorer, TermQuery, Weight,
    },
    schema::{Field, IndexRecordOption},
    tokenizer::{
        AsciiFoldingFilter, LowerCaser, RawTokenizer, RemoveLongFilter, SimpleTokenizer,
        TextAnalyzer,
    },
    DocId, DocSet, Postings, Score, Searcher, SegmentReader, Term, TERMINATED,
};

const MAX_QUERY_BYTES: usize = 8192;
const MAX_TOKENS: usize = 512;
const MAX_DEPTH: usize = 32;
const MAX_DISTANCE: u32 = 1024;
const MAX_EXPANDED_TERMS: usize = 128;
const MAX_DICTIONARY_VISITS: usize = 100_000;

/// The same analyzer must be used on both sides of the index boundary.
/// Accent folding keeps non-Latin scripts intact; transliteration does not.
pub fn text_analyzer() -> TextAnalyzer {
    TextAnalyzer::builder(SimpleTokenizer::default())
        .filter(RemoveLongFilter::limit(40))
        .filter(LowerCaser)
        .filter(AsciiFoldingFilter)
        .build()
}

fn analyzed_tokens(text: &str) -> Vec<(usize, String)> {
    let mut analyzer = text_analyzer();
    let mut stream = analyzer.token_stream(text);
    let mut tokens = Vec::new();
    while stream.advance() {
        let token = stream.token();
        tokens.push((token.position, token.text.clone()));
    }
    tokens
}

pub fn simple_tokenize(text: &str) -> Vec<String> {
    analyzed_tokens(text)
        .into_iter()
        .map(|(_, text)| text)
        .collect()
}

/// Fold a wildcard's literal text without losing its operators or changing
/// scripts. RawTokenizer deliberately keeps the entire pattern together.
pub fn fold_accents(text: &str) -> String {
    let mut analyzer = TextAnalyzer::builder(RawTokenizer::default())
        .filter(LowerCaser)
        .filter(AsciiFoldingFilter)
        .build();
    let mut stream = analyzer.token_stream(text);
    if stream.advance() {
        stream.token().text.clone()
    } else {
        String::new()
    }
}

#[derive(Clone)]
pub struct SearchFields {
    pub title: Field,
    pub body: Field,
    pub headings: Field,
    pub body_translated: Option<Field>,
}

impl SearchFields {
    pub fn weighted(&self) -> Vec<(Field, f32)> {
        let mut result = vec![(self.title, 3.0), (self.headings, 2.0), (self.body, 1.0)];
        if let Some(field) = self.body_translated {
            result.push((field, 0.7));
        }
        result
    }

    fn resolve(&self, name: &str) -> Result<Field> {
        match name.to_ascii_lowercase().as_str() {
            "title" => Ok(self.title),
            "body" | "text" => Ok(self.body),
            "headings" | "h" => Ok(self.headings),
            "translated" | "body_translated" => self.body_translated.ok_or_else(|| {
                anyhow!("This index has no translated field. Rebuild it to search translations.")
            }),
            _ => bail!("Unknown search field '{name}'. Use title, body, headings, or translated."),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Kind {
    Word(String),
    Phrase(String),
    Field(String),
    And,
    Or,
    Not,
    Near(u32, bool),
    LParen,
    RParen,
    End,
}

#[derive(Debug, Clone)]
struct Token {
    kind: Kind,
    start: usize,
    end: usize,
}

fn lex(input: &str) -> Result<Vec<Token>> {
    if input.len() > MAX_QUERY_BYTES {
        bail!("Query is too long (maximum {MAX_QUERY_BYTES} bytes).");
    }
    let mut out = Vec::new();
    let mut chars = input.char_indices().peekable();
    while let Some((start, ch)) = chars.next() {
        if ch.is_whitespace() {
            continue;
        }
        let kind = match ch {
            '(' => Kind::LParen,
            ')' => Kind::RParen,
            ':' => bail!("A field name is required before ':' at byte {}.", start + 1),
            '"' => {
                let mut value = String::new();
                let mut closed = false;
                while let Some((_, ch)) = chars.next() {
                    match ch {
                        '"' => {
                            closed = true;
                            break;
                        }
                        '\\' => match chars.next() {
                            Some((_, escaped @ ('"' | '\\'))) => value.push(escaped),
                            Some((_, other)) => {
                                value.push('\\');
                                value.push(other);
                            }
                            None => break,
                        },
                        other => value.push(other),
                    }
                }
                if !closed {
                    bail!("Unclosed quotation mark at byte {}.", start + 1);
                }
                Kind::Phrase(value)
            }
            _ => {
                let mut value = String::from(ch);
                while let Some(&(_, next)) = chars.peek() {
                    if next.is_whitespace() || matches!(next, '(' | ')' | '"' | ':') {
                        break;
                    }
                    chars.next();
                    value.push(next);
                }
                if chars.peek().is_some_and(|(_, ch)| *ch == ':') {
                    chars.next();
                    Kind::Field(value)
                } else if value.eq_ignore_ascii_case("AND") {
                    Kind::And
                } else if value.eq_ignore_ascii_case("OR") {
                    Kind::Or
                } else if value.eq_ignore_ascii_case("NOT") {
                    Kind::Not
                } else {
                    let lower = value.to_ascii_lowercase();
                    if lower.starts_with("w/") || lower.starts_with("pre/") {
                        let (n, ordered) = if let Some(n) = lower.strip_prefix("pre/") {
                            (n, true)
                        } else {
                            (&lower[2..], false)
                        };
                        let n: u32 = n.parse().map_err(|_| {
                            anyhow!("Invalid proximity operator '{value}'. Use w/N or pre/N.")
                        })?;
                        if n > MAX_DISTANCE {
                            bail!("Proximity distance must be between 0 and {MAX_DISTANCE}.");
                        }
                        Kind::Near(n, ordered)
                    } else {
                        Kind::Word(value)
                    }
                }
            }
        };
        let end = chars.peek().map_or(input.len(), |(at, _)| *at);
        out.push(Token { kind, start, end });
        if out.len() > MAX_TOKENS {
            bail!("Query has too many tokens (maximum {MAX_TOKENS}).");
        }
    }
    out.push(Token {
        kind: Kind::End,
        start: input.len(),
        end: input.len(),
    });
    Ok(out)
}

#[derive(Clone, Debug)]
enum Expr {
    Word(String),
    Phrase(String),
    Scope(Field, Box<Expr>),
    And(Vec<Expr>),
    Or(Vec<Expr>),
    Not(Box<Expr>),
    Near(Box<Expr>, Box<Expr>, u32, bool),
}

struct Parser<'a> {
    tokens: Vec<Token>,
    at: usize,
    fields: &'a SearchFields,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> &Kind {
        &self.tokens[self.at].kind
    }
    fn next(&mut self) -> Kind {
        let token = self.peek().clone();
        self.at += 1;
        token
    }
    fn location(&self) -> usize {
        self.tokens[self.at].start + 1
    }
    fn depth(depth: usize) -> Result<()> {
        if depth > MAX_DEPTH {
            bail!("Query nesting is too deep (maximum {MAX_DEPTH}).");
        }
        Ok(())
    }
    fn parse_or(&mut self, depth: usize) -> Result<Expr> {
        Self::depth(depth)?;
        let mut parts = vec![self.parse_and(depth)?];
        while matches!(self.peek(), Kind::Or) {
            self.next();
            parts.push(self.parse_and(depth)?);
        }
        Ok(if parts.len() == 1 {
            parts.remove(0)
        } else {
            Expr::Or(parts)
        })
    }
    fn parse_and(&mut self, depth: usize) -> Result<Expr> {
        let mut parts = vec![self.parse_not(depth)?];
        loop {
            if matches!(self.peek(), Kind::And) {
                self.next();
            } else if !matches!(
                self.peek(),
                Kind::Word(_) | Kind::Phrase(_) | Kind::Field(_) | Kind::LParen | Kind::Not
            ) {
                break;
            }
            parts.push(self.parse_not(depth)?);
        }
        Ok(if parts.len() == 1 {
            parts.remove(0)
        } else {
            Expr::And(parts)
        })
    }
    fn parse_not(&mut self, depth: usize) -> Result<Expr> {
        Self::depth(depth)?;
        if matches!(self.peek(), Kind::Not) {
            self.next();
            return Ok(Expr::Not(Box::new(self.parse_not(depth + 1)?)));
        }
        let lhs = self.atom(depth)?;
        if let Kind::Near(n, ordered) = self.peek().clone() {
            self.next();
            let rhs = self.atom(depth)?;
            if matches!(self.peek(), Kind::Near(_, _)) {
                bail!("Chained proximity needs separate clauses, for example (a pre/3 b) AND (b pre/3 c).");
            }
            Ok(Expr::Near(Box::new(lhs), Box::new(rhs), n, ordered))
        } else {
            Ok(lhs)
        }
    }
    fn atom(&mut self, depth: usize) -> Result<Expr> {
        Self::depth(depth)?;
        match self.peek().clone() {
            Kind::Word(word) => {
                self.next();
                Ok(Expr::Word(word))
            }
            Kind::Phrase(phrase) => {
                self.next();
                Ok(Expr::Phrase(phrase))
            }
            Kind::Field(name) => {
                self.next();
                let field = self.fields.resolve(&name)?;
                Ok(Expr::Scope(field, Box::new(self.atom(depth + 1)?)))
            }
            Kind::LParen => {
                self.next();
                let expr = self.parse_or(depth + 1)?;
                if !matches!(self.peek(), Kind::RParen) {
                    bail!("Missing closing ')' at byte {}.", self.location());
                }
                self.next();
                Ok(expr)
            }
            Kind::End => bail!("Expected a search term at the end of the query."),
            other => bail!(
                "Expected a search term at byte {}, found {other:?}.",
                self.location()
            ),
        }
    }
}

/// Parse the entire input before constructing queries. Nothing after a closing
/// group, and no incomplete operand, can be silently discarded.
pub fn translate(
    query: &str,
    searcher: &Searcher,
    fields: &SearchFields,
) -> Result<Box<dyn Query>> {
    let expr = parse_query(query, fields)?;
    compile(
        &expr,
        fields,
        searcher,
        None,
        &mut ExpansionBudget::default(),
    )
}

fn parse_query(query: &str, fields: &SearchFields) -> Result<Expr> {
    let mut parser = Parser {
        tokens: lex(query)?,
        at: 0,
        fields,
    };
    let expr = parser.parse_or(0)?;
    if !matches!(parser.peek(), Kind::End) {
        bail!(
            "Unexpected token at byte {}. Check parentheses and operators.",
            parser.location()
        );
    }
    validate_expr(&expr)?;
    Ok(expr)
}

fn validate_expr(expr: &Expr) -> Result<()> {
    match expr {
        Expr::Word(word) => {
            WordPattern::parse(word)?;
        }
        Expr::Phrase(text) => {
            if analyzed_tokens(text).is_empty() {
                bail!("A phrase must contain an indexed word.");
            }
        }
        Expr::Scope(_, inner) | Expr::Not(inner) => validate_expr(inner)?,
        Expr::And(parts) | Expr::Or(parts) => {
            for part in parts {
                validate_expr(part)?;
            }
        }
        Expr::Near(left, right, _, _) => {
            validate_proximity_operand(left)?;
            validate_proximity_operand(right)?;
        }
    }
    Ok(())
}

/// Hybrid retrieval may broaden ordinary words semantically, but an explicit
/// condition is a constraint on every result, including vector-only candidates.
/// This check also rejects malformed input before an embedding model is invoked.
pub fn has_explicit_syntax(query: &str) -> Result<bool> {
    let fields = SearchFields {
        title: Field::from_field_id(0),
        body: Field::from_field_id(1),
        headings: Field::from_field_id(2),
        body_translated: Some(Field::from_field_id(3)),
    };
    parse_query(query, &fields)?;
    Ok(lex(query)?.iter().any(|token| match &token.kind {
        Kind::Word(word) => word.contains(['*', '?', '~']),
        Kind::End => false,
        _ => true,
    }))
}

/// Positive, literal source-body terms for choosing an initial stored passage.
/// This is a display heuristic, not an explanation of phrase/proximity or
/// expanded matches. Operators, exclusions, and non-body field constraints do
/// not become snippet keywords; fuzzy/wildcard terms have no invented literal.
pub fn literal_body_terms(query: &str) -> Result<Vec<String>> {
    let fields = SearchFields {
        title: Field::from_field_id(0), body: Field::from_field_id(1),
        headings: Field::from_field_id(2), body_translated: Some(Field::from_field_id(3)),
    };
    fn collect(expr: &Expr, body: Field, scope: Option<Field>, positive: bool, terms: &mut BTreeSet<String>) -> Result<()> {
        match expr {
            Expr::Not(inner) => collect(inner, body, scope, !positive, terms)?,
            Expr::Scope(field, inner) => collect(inner, body, Some(*field), positive, terms)?,
            Expr::And(parts) | Expr::Or(parts) => {
                for part in parts { collect(part, body, scope, positive, terms)?; }
            }
            Expr::Near(left, right, _, _) => {
                collect(left, body, scope, positive, terms)?;
                collect(right, body, scope, positive, terms)?;
            }
            Expr::Word(word) if positive && scope.is_none_or(|field| field == body) => {
                if let WordPattern::Exact(tokens) = WordPattern::parse(word)? {
                    terms.extend(tokens.into_iter().map(|(_, text)| text));
                }
            }
            Expr::Phrase(text) if positive && scope.is_none_or(|field| field == body) => {
                terms.extend(simple_tokenize(text));
            }
            _ => {}
        }
        Ok(())
    }
    let expr = parse_query(query, &fields)?;
    let mut terms = BTreeSet::new();
    collect(&expr, fields.body, None, true, &mut terms)?;
    Ok(terms.into_iter().collect())
}

fn union(queries: Vec<Box<dyn Query>>) -> Box<dyn Query> {
    if queries.is_empty() {
        Box::new(EmptyQuery)
    } else if queries.len() == 1 {
        queries.into_iter().next().unwrap()
    } else {
        Box::new(BooleanQuery::new(
            queries.into_iter().map(|q| (Occur::Should, q)).collect(),
        ))
    }
}
fn intersection(queries: Vec<Box<dyn Query>>) -> Box<dyn Query> {
    if queries.len() == 1 {
        queries.into_iter().next().unwrap()
    } else {
        Box::new(BooleanQuery::new(
            queries.into_iter().map(|q| (Occur::Must, q)).collect(),
        ))
    }
}
fn boosted(query: Box<dyn Query>, boost: f32) -> Box<dyn Query> {
    if boost == 1.0 {
        query
    } else {
        Box::new(BoostQuery::new(query, boost))
    }
}
fn targets(fields: &SearchFields, scope: Option<Field>) -> Vec<(Field, f32)> {
    scope.map_or_else(|| fields.weighted(), |field| vec![(field, 1.0)])
}

fn compile(
    expr: &Expr,
    fields: &SearchFields,
    searcher: &Searcher,
    scope: Option<Field>,
    budget: &mut ExpansionBudget,
) -> Result<Box<dyn Query>> {
    match expr {
        Expr::And(parts) => Ok(intersection(
            parts
                .iter()
                .map(|x| compile(x, fields, searcher, scope, budget))
                .collect::<Result<_>>()?,
        )),
        Expr::Or(parts) => Ok(union(
            parts
                .iter()
                .map(|x| compile(x, fields, searcher, scope, budget))
                .collect::<Result<_>>()?,
        )),
        Expr::Not(inner) => Ok(Box::new(BooleanQuery::new(vec![
            (Occur::Must, Box::new(AllQuery)),
            (
                Occur::MustNot,
                compile(inner, fields, searcher, scope, budget)?,
            ),
        ]))),
        Expr::Scope(field, inner) => compile(inner, fields, searcher, Some(*field), budget),
        Expr::Word(word) => {
            if word == "*" && scope.is_none() {
                return Ok(Box::new(AllQuery));
            }
            let pattern = WordPattern::parse(word)?;
            let queries = targets(fields, scope)
                .into_iter()
                .map(|(field, boost)| Ok(boosted(pattern.query(field)?, boost)))
                .collect::<Result<_>>()?;
            Ok(union(queries))
        }
        Expr::Phrase(text) => {
            let words = analyzed_tokens(text);
            if words.is_empty() {
                bail!(
                    "A phrase must contain an indexed word (words must be shorter than 40 bytes)."
                );
            }
            Ok(union(
                targets(fields, scope)
                    .into_iter()
                    .map(|(field, boost)| boosted(sequence_query(&terms_for(field, &words)), boost))
                    .collect(),
            ))
        }
        Expr::Near(left, right, distance, ordered) => {
            // Reject unsupported Boolean shapes before field filtering could
            // hide them. A field mismatch is a valid query with no matches.
            validate_proximity_operand(left)?;
            validate_proximity_operand(right)?;
            let mut queries = Vec::new();
            for (field, boost) in targets(fields, scope) {
                let left = proximity_sequences(left, field, searcher, budget)?;
                let right = proximity_sequences(right, field, searcher, budget)?;
                if !left.is_empty() && !right.is_empty() {
                    queries.push(boosted(
                        Box::new(PositionalQuery {
                            left,
                            right,
                            distance: *distance,
                            ordered: *ordered,
                        }),
                        boost,
                    ));
                }
            }
            Ok(union(queries))
        }
    }
}

#[derive(Clone, Debug)]
enum WordPattern {
    Exact(Vec<(usize, String)>),
    Wildcard(String),
    Fuzzy(String, u8),
}
impl WordPattern {
    fn parse(word: &str) -> Result<Self> {
        if let Some((base, suffix)) = word.rsplit_once('~') {
            if base.is_empty() || base.contains(['*', '?', '~']) {
                bail!("Fuzzy matching needs one plain word followed by ~, ~1, or ~2.");
            }
            let distance = if suffix.is_empty() {
                1
            } else {
                suffix
                    .parse::<u8>()
                    .map_err(|_| anyhow!("Fuzzy distance must be 0, 1, or 2."))?
            };
            if distance > 2 {
                bail!("Fuzzy distance must be 0, 1, or 2.");
            }
            let tokens = simple_tokenize(base);
            if tokens.len() != 1 {
                bail!("Fuzzy matching applies to one indexed word.");
            }
            return Ok(Self::Fuzzy(tokens[0].clone(), distance));
        }
        if word.contains(['*', '?']) {
            if word.chars().filter(|c| matches!(c, '*' | '?')).count() > 32 {
                bail!("A wildcard pattern may contain at most 32 wildcard symbols.");
            }
            if word
                .chars()
                .any(|c| !c.is_alphanumeric() && c != '*' && c != '?')
            {
                bail!("A wildcard pattern must contain letters, numbers, '*' or '?'. Use separate words for punctuation.");
            }
            return Ok(Self::Wildcard(fold_accents(word)));
        }
        let tokens = analyzed_tokens(word);
        if tokens.is_empty() {
            bail!(
                "'{word}' contains no indexed word. Indexed words must be shorter than 40 bytes."
            );
        }
        Ok(Self::Exact(tokens))
    }
    fn query(&self, field: Field) -> Result<Box<dyn Query>> {
        Ok(match self {
            Self::Exact(tokens) => intersection(
                tokens
                    .iter()
                    .map(|(_, token)| {
                        Box::new(TermQuery::new(
                            Term::from_field_text(field, token),
                            IndexRecordOption::WithFreqsAndPositions,
                        )) as Box<dyn Query>
                    })
                    .collect(),
            ),
            Self::Wildcard(pattern) => Box::new(
                RegexQuery::from_pattern(&wildcard_to_regex(pattern), field)
                    .map_err(|e| anyhow!("Invalid wildcard: {e}"))?,
            ),
            Self::Fuzzy(term, distance) => Box::new(FuzzyTermQuery::new(
                Term::from_field_text(field, term),
                *distance,
                true,
            )),
        })
    }
}

fn wildcard_to_regex(pattern: &str) -> String {
    let mut regex = String::new();
    for ch in pattern.chars() {
        match ch {
            '*' => regex.push_str(".*"),
            '?' => regex.push('.'),
            c if ".+^${}()|[]\\".contains(c) => {
                regex.push('\\');
                regex.push(c);
            }
            c => regex.push(c),
        }
    }
    regex
}

type Sequence = Vec<(usize, Term)>;
fn terms_for(field: Field, tokens: &[(usize, String)]) -> Sequence {
    let first = tokens.first().map_or(0, |(position, _)| *position);
    tokens
        .iter()
        .map(|(position, token)| (position - first, Term::from_field_text(field, token)))
        .collect()
}
fn sequence_query(sequence: &Sequence) -> Box<dyn Query> {
    if sequence.len() == 1 {
        Box::new(TermQuery::new(
            sequence[0].1.clone(),
            IndexRecordOption::WithFreqsAndPositions,
        ))
    } else {
        Box::new(PhraseQuery::new_with_offset(sequence.clone()))
    }
}
fn validate_proximity_operand(expr: &Expr) -> Result<()> {
    match expr {
        Expr::Word(word) => { WordPattern::parse(word)?; Ok(()) }
        Expr::Phrase(text) => if analyzed_tokens(text).is_empty() { bail!("A proximity phrase cannot be empty."); } else { Ok(()) },
        Expr::Scope(_, inner) => validate_proximity_operand(inner),
        Expr::Or(parts) => { for part in parts { validate_proximity_operand(part)?; } Ok(()) }
        _ => bail!("Proximity operands support words, phrases, and OR alternatives. Put AND/NOT or another proximity operator in a separate clause."),
    }
}

#[derive(Default)]
struct ExpansionBudget {
    visited: usize,
    expanded: usize,
}
fn proximity_sequences(
    expr: &Expr,
    field: Field,
    searcher: &Searcher,
    budget: &mut ExpansionBudget,
) -> Result<Vec<Sequence>> {
    match expr {
        Expr::Scope(required, inner) => {
            if *required == field {
                proximity_sequences(inner, field, searcher, budget)
            } else {
                Ok(Vec::new())
            }
        }
        Expr::Or(parts) => {
            let mut out = Vec::new();
            for part in parts {
                out.extend(proximity_sequences(part, field, searcher, budget)?);
            }
            Ok(out)
        }
        Expr::Phrase(text) => Ok(vec![terms_for(field, &analyzed_tokens(text))]),
        Expr::Word(word) => match WordPattern::parse(word)? {
            WordPattern::Exact(tokens) => Ok(vec![terms_for(field, &tokens)]),
            pattern => {
                let prefix: String = match &pattern {
                    WordPattern::Wildcard(text) => text
                        .chars()
                        .take_while(|ch| *ch != '*' && *ch != '?')
                        .collect(),
                    _ => String::new(),
                };
                let mut terms = BTreeSet::new();
                for segment in searcher.segment_readers() {
                    let index = segment.inverted_index(field)?;
                    let mut stream = index.terms().range().ge(prefix.as_bytes()).into_stream()?;
                    while stream.advance() {
                        let text = std::str::from_utf8(stream.key())?;
                        if !text.starts_with(&prefix) {
                            break;
                        }
                        budget.visited += 1;
                        if budget.visited > MAX_DICTIONARY_VISITS {
                            bail!("Proximity expansion examined too many indexed words. Use a longer wildcard prefix or an exact term.");
                        }
                        let matches = match &pattern {
                            WordPattern::Wildcard(pattern) => wildcard_matches(pattern, text),
                            WordPattern::Fuzzy(term, distance) => {
                                edit_distance(term, text, *distance as usize) <= *distance as usize
                            }
                            _ => unreachable!(),
                        };
                        if matches && terms.insert(text.to_owned()) {
                            budget.expanded += 1;
                            if budget.expanded > MAX_EXPANDED_TERMS {
                                bail!("Proximity expansion matches too many words (maximum {MAX_EXPANDED_TERMS}). Narrow the pattern or field.");
                            }
                        }
                    }
                }
                Ok(terms
                    .into_iter()
                    .map(|text| vec![(0, Term::from_field_text(field, &text))])
                    .collect())
            }
        },
        _ => unreachable!("validated before expansion"),
    }
}

fn wildcard_matches(pattern: &str, value: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let value: Vec<char> = value.chars().collect();
    let (mut p, mut v, mut star, mut retry) = (0, 0, None, 0);
    while v < value.len() {
        if p < pattern.len() && (pattern[p] == '?' || pattern[p] == value[v]) {
            p += 1;
            v += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            star = Some(p);
            p += 1;
            retry = v;
        } else if let Some(at) = star {
            p = at + 1;
            retry += 1;
            v = retry;
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == '*' {
        p += 1;
    }
    p == pattern.len()
}

/// Optimal-string-alignment distance: one adjacent transposition costs one,
/// matching the FuzzyTermQuery setting used for ordinary word queries.
fn edit_distance(left: &str, right: &str, limit: usize) -> usize {
    let left: Vec<char> = left.chars().collect();
    let right: Vec<char> = right.chars().collect();
    if left.len().abs_diff(right.len()) > limit {
        return limit + 1;
    }
    let mut older = vec![0; right.len() + 1];
    let mut previous: Vec<usize> = (0..=right.len()).collect();
    for (i, a) in left.iter().enumerate() {
        let mut current = vec![i + 1; right.len() + 1];
        for (j, b) in right.iter().enumerate() {
            current[j + 1] = (current[j] + 1)
                .min(previous[j + 1] + 1)
                .min(previous[j] + usize::from(a != b));
            if i > 0 && j > 0 && *a == right[j - 1] && left[i - 1] == *b {
                current[j + 1] = current[j + 1].min(older[j - 1] + 1);
            }
        }
        older = previous;
        previous = current;
    }
    previous[right.len()]
}

/// Uses actual token positions, because phrase slop permits transpositions and
/// cannot express a strict before relationship. Candidate retrieval still uses
/// the ordinary inverted index and BM25; only candidates need position checks.
#[derive(Clone, Debug)]
struct PositionalQuery {
    left: Vec<Sequence>,
    right: Vec<Sequence>,
    distance: u32,
    ordered: bool,
}
impl Query for PositionalQuery {
    fn weight(&self, scoring: EnableScoring<'_>) -> tantivy::Result<Box<dyn Weight>> {
        let candidates = intersection(vec![
            union(self.left.iter().map(sequence_query).collect()),
            union(self.right.iter().map(sequence_query).collect()),
        ]);
        Ok(Box::new(PositionalWeight {
            query: self.clone(),
            candidates: candidates.weight(scoring)?,
        }))
    }
    fn query_terms<'a>(&'a self, visitor: &mut dyn FnMut(&'a Term, bool)) {
        for sequence in self.left.iter().chain(self.right.iter()) {
            for (_, term) in sequence {
                visitor(term, true);
            }
        }
    }
}
struct PositionalWeight {
    query: PositionalQuery,
    candidates: Box<dyn Weight>,
}
impl Weight for PositionalWeight {
    fn scorer(&self, reader: &SegmentReader, boost: Score) -> tantivy::Result<Box<dyn Scorer>> {
        let mut scorer = PositionalScorer {
            candidates: self.candidates.scorer(reader, boost)?,
            left: bind_sequences(&self.query.left, reader)?,
            right: bind_sequences(&self.query.right, reader)?,
            distance: self.query.distance,
            ordered: self.query.ordered,
        };
        scorer.skip_nonmatching();
        Ok(Box::new(scorer))
    }
    fn explain(&self, reader: &SegmentReader, doc: DocId) -> tantivy::Result<Explanation> {
        let mut scorer = self.scorer(reader, 1.0)?;
        if scorer.seek(doc) != doc {
            return Err(tantivy::TantivyError::InvalidArgument(
                "Document does not match the positional query".into(),
            ));
        }
        Ok(Explanation::new(
            "Positional match with candidate BM25 score",
            scorer.score(),
        ))
    }
}

type BoundSequence = Vec<(u32, Option<SegmentPostings>)>;
fn bind_sequences(
    sequences: &[Sequence],
    reader: &SegmentReader,
) -> tantivy::Result<Vec<BoundSequence>> {
    sequences
        .iter()
        .map(|sequence| {
            sequence
                .iter()
                .map(|(offset, term)| {
                    let postings = reader
                        .inverted_index(term.field())?
                        .read_postings(term, IndexRecordOption::WithFreqsAndPositions)?;
                    Ok((*offset as u32, postings))
                })
                .collect()
        })
        .collect()
}
fn spans(sequences: &mut [BoundSequence], doc: DocId) -> Vec<(u32, u32)> {
    let mut result = Vec::new();
    for sequence in sequences {
        let mut positions = Vec::new();
        let mut starts: Vec<u32> = Vec::new();
        let last_offset = sequence.last().map_or(0, |(offset, _)| *offset);
        for (i, (offset, postings)) in sequence.iter_mut().enumerate() {
            let Some(postings) = postings else {
                starts.clear();
                break;
            };
            if postings.doc() < doc {
                postings.seek(doc);
            }
            if postings.doc() != doc {
                starts.clear();
                break;
            }
            postings.positions(&mut positions);
            if i == 0 {
                starts.clone_from(&positions);
            } else {
                starts.retain(|start| {
                    start
                        .checked_add(*offset)
                        .is_some_and(|at| positions.binary_search(&at).is_ok())
                });
            }
            if starts.is_empty() {
                break;
            }
        }
        result.extend(
            starts
                .into_iter()
                .filter_map(|start| start.checked_add(last_offset).map(|end| (start, end))),
        );
    }
    result.sort_unstable();
    result.dedup();
    result
}
fn before_within(left: &[(u32, u32)], right: &[(u32, u32)], distance: u32) -> bool {
    left.iter().any(|(_, end)| {
        let next = right.partition_point(|(start, _)| start <= end);
        right
            .get(next)
            .is_some_and(|(start, _)| start - end - 1 <= distance)
    })
}
struct PositionalScorer {
    candidates: Box<dyn Scorer>,
    left: Vec<BoundSequence>,
    right: Vec<BoundSequence>,
    distance: u32,
    ordered: bool,
}
impl PositionalScorer {
    fn skip_nonmatching(&mut self) -> DocId {
        loop {
            let doc = self.candidates.doc();
            if doc == TERMINATED {
                return doc;
            }
            let left = spans(&mut self.left, doc);
            let right = spans(&mut self.right, doc);
            if before_within(&left, &right, self.distance)
                || (!self.ordered && before_within(&right, &left, self.distance))
            {
                return doc;
            }
            self.candidates.advance();
        }
    }
}
impl DocSet for PositionalScorer {
    fn advance(&mut self) -> DocId {
        self.candidates.advance();
        self.skip_nonmatching()
    }
    fn seek(&mut self, target: DocId) -> DocId {
        if self.candidates.doc() < target {
            self.candidates.seek(target);
        }
        self.skip_nonmatching()
    }
    fn doc(&self) -> DocId {
        self.candidates.doc()
    }
    fn size_hint(&self) -> u32 {
        self.candidates.size_hint()
    }
}
impl Scorer for PositionalScorer {
    fn score(&mut self) -> Score {
        self.candidates.score()
    }
}

/// Rewrite complete bare-word tokens while retaining every original separator,
/// quoted phrase, escaped quote, field prefix, and operator. The replacement
/// cannot accidentally enter a phrase that begins after a field name.
pub fn rewrite_bare_words(
    query: &str,
    mut replacement: impl FnMut(&str) -> Option<String>,
) -> String {
    let Ok(tokens) = lex(query) else {
        return query.to_owned();
    };
    let mut result = String::new();
    let mut copied = 0;
    for (i, token) in tokens.iter().enumerate() {
        let Kind::Word(word) = &token.kind else {
            continue;
        };
        if i > 0 && matches!(tokens[i - 1].kind, Kind::Field(_)) {
            continue;
        }
        if let Some(value) = replacement(word) {
            result.push_str(&query[copied..token.start]);
            result.push_str(&value);
            copied = token.end;
        }
    }
    result.push_str(&query[copied..]);
    result
}

/// Add ~1 to unmodified bare alphabetic words, including words inside groups.
/// Invalid input is left unchanged so validation reports the original error.
pub fn fuzzify_query(query: &str) -> String {
    rewrite_bare_words(query, |word| {
        (word.chars().count() >= 4 && word.chars().all(char::is_alphabetic))
            .then(|| format!("{word}~1"))
    })
}
