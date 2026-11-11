//! Clause truth comes from the actual indexed document. Source ranges are
//! separately derived from the selected stored passage using the index analyzer.
//! A matching clause may therefore have no ranges in this particular passage.
use super::*;
use anyhow::ensure;
use levenshtein_automata::{Distance, LevenshteinAutomatonBuilder, DFA};
use tantivy::{tokenizer::Token as AnalyzedToken, DocAddress};

pub const MAX_EVIDENCE_SPANS: usize = 256;
const MAX_PASSAGE_BYTES: usize = 256 * 1024;
const MAX_EVIDENCE_WORK: usize = 4_000_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvidenceSpan {
    pub start: usize,
    pub end: usize,
    pub kind: String,
    pub clause: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExplainClause {
    pub kind: String,
    pub label: String,
    pub matched: bool,
    pub field: Option<String>,
    pub matched_fields: Vec<String>,
    pub children: Vec<ExplainClause>,
}

#[derive(Clone, Debug)]
pub struct QueryEvidence {
    pub matched: bool,
    pub clauses: ExplainClause,
    pub spans: Vec<EvidenceSpan>,
    pub spans_truncated: bool,
}

/// Whether the returned source text includes the index's complete edge token.
/// Interior token ranges are still usable when a chunk clips an edge word.
#[derive(Clone, Copy, Debug)]
pub struct PassageEdges {
    pub start: bool,
    pub end: bool,
}

struct Evaluated {
    clause: ExplainClause,
    children: Vec<Evaluated>,
    spans: Vec<EvidenceSpan>,
    spans_truncated: bool,
}

struct Context<'a> {
    searcher: &'a Searcher,
    fields: &'a SearchFields,
    address: DocAddress,
    tokens: Vec<AnalyzedToken>,
    expansion: ExpansionBudget,
    work: usize,
    edges: PassageEdges,
    text_bytes: usize,
    span_scan_truncated: bool,
}

/// Evaluate the complete parsed expression on one live index document and
/// return contributing, positive source ranges from the selected body passage.
/// No source range means neither failure nor a semantic word attribution.
pub fn explain(
    query: &str,
    searcher: &Searcher,
    fields: &SearchFields,
    address: DocAddress,
    passage: &str,
    edges: PassageEdges,
) -> Result<QueryEvidence> {
    ensure!(
        passage.len() <= MAX_PASSAGE_BYTES,
        "Explanation passage exceeds 256 KiB"
    );
    let segment = searcher
        .segment_readers()
        .get(address.segment_ord as usize)
        .ok_or_else(|| anyhow!("Explanation document is no longer indexed"))?;
    ensure!(
        address.doc_id < segment.max_doc()
            && segment
                .alive_bitset()
                .is_none_or(|alive| alive.is_alive(address.doc_id)),
        "Explanation document is no longer indexed"
    );
    let expr = parse_query(query, fields)?;
    let mut analyzer = text_analyzer();
    let mut stream = analyzer.token_stream(passage);
    let mut tokens = Vec::new();
    while stream.advance() {
        tokens.push(stream.token().clone());
    }
    let mut context = Context {
        searcher,
        fields,
        address,
        tokens,
        expansion: ExpansionBudget::default(),
        work: 0,
        edges,
        text_bytes: passage.len(),
        span_scan_truncated: false,
    };
    let evaluated = context.evaluate(&expr, None)?;
    let mut spans = Vec::new();
    let mut truncated = false;
    if evaluated.clause.matched {
        collect_contributing(&evaluated, true, &mut spans, &mut truncated);
    }
    spans.sort_by(|a, b| {
        (a.start, a.end, &a.kind, &a.clause).cmp(&(b.start, b.end, &b.kind, &b.clause))
    });
    spans.dedup();
    if spans.len() > MAX_EVIDENCE_SPANS {
        spans.truncate(MAX_EVIDENCE_SPANS);
        truncated = true;
    }
    Ok(QueryEvidence {
        matched: evaluated.clause.matched,
        clauses: evaluated.clause,
        spans,
        spans_truncated: truncated,
    })
}

fn collect_contributing(
    node: &Evaluated,
    expected: bool,
    spans: &mut Vec<EvidenceSpan>,
    truncated: &mut bool,
) {
    if node.clause.matched != expected {
        return;
    }
    match node.clause.kind.as_str() {
        "not" => collect_contributing(&node.children[0], !expected, spans, truncated),
        "and" | "or" | "field" => {
            for child in &node.children {
                if child.clause.matched == expected {
                    collect_contributing(child, expected, spans, truncated);
                }
            }
        }
        _ if expected => {
            *truncated |= node.spans_truncated;
            for span in &node.spans {
                if spans.len() < MAX_EVIDENCE_SPANS + 1 {
                    spans.push(span.clone());
                } else {
                    *truncated = true;
                }
            }
        }
        _ => {}
    }
}

fn label(text: &str) -> String {
    if text.len() <= 512 {
        return text.to_owned();
    }
    let mut end = 509;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

fn field_name(fields: &SearchFields, field: Field) -> &'static str {
    if field == fields.title {
        "title"
    } else if field == fields.body {
        "body"
    } else if field == fields.headings {
        "headings"
    } else {
        "translated"
    }
}

fn operand_label(expr: &Expr, fields: &SearchFields) -> String {
    match expr {
        Expr::Word(word) => word.clone(),
        Expr::Phrase(text) => format!("\"{text}\""),
        Expr::Scope(field, inner) => format!(
            "{}:{}",
            field_name(fields, *field),
            operand_label(inner, fields)
        ),
        Expr::Or(parts) => format!(
            "({})",
            parts
                .iter()
                .map(|part| operand_label(part, fields))
                .collect::<Vec<_>>()
                .join(" OR ")
        ),
        _ => unreachable!("only validated proximity operands"),
    }
}

impl Context<'_> {
    fn charge(&mut self, amount: usize) -> Result<()> {
        self.work = self.work.saturating_add(amount);
        ensure!(self.work <= MAX_EVIDENCE_WORK,
            "This passage/query exceeds the bounded explanation work limit; inspect a shorter passage or narrower query");
        Ok(())
    }

    fn matches(&self, query: &dyn Query) -> Result<bool> {
        let weight = query.weight(EnableScoring::disabled_from_searcher(self.searcher))?;
        let segment = self.searcher.segment_reader(self.address.segment_ord);
        let mut scorer = weight.scorer(segment, 1.0)?;
        if scorer.doc() < self.address.doc_id {
            scorer.seek(self.address.doc_id);
        }
        Ok(scorer.doc() == self.address.doc_id)
    }

    fn evaluate(&mut self, expr: &Expr, scope: Option<Field>) -> Result<Evaluated> {
        let mut children = Vec::new();
        let (kind, text, matched) = match expr {
            Expr::And(parts) | Expr::Or(parts) => {
                for part in parts {
                    children.push(self.evaluate(part, scope)?);
                }
                let and = matches!(expr, Expr::And(_));
                let matched = if and {
                    children.iter().all(|x| x.clause.matched)
                } else {
                    children.iter().any(|x| x.clause.matched)
                };
                (
                    if and { "and" } else { "or" },
                    if and { "AND".into() } else { "OR".into() },
                    matched,
                )
            }
            Expr::Not(inner) => {
                let child = self.evaluate(inner, scope)?;
                let matched = !child.clause.matched;
                children.push(child);
                ("not", "NOT".into(), matched)
            }
            Expr::Scope(field, inner) => {
                let child = self.evaluate(inner, Some(*field))?;
                let matched = child.clause.matched;
                children.push(child);
                ("field", field_name(self.fields, *field).into(), matched)
            }
            _ => {
                let query = compile(expr, self.fields, self.searcher, scope, &mut self.expansion)?;
                let matched = self.matches(query.as_ref())?;
                let (kind, text) = match expr {
                    Expr::Word(word) if word == "*" && scope.is_none() => {
                        ("all", "All indexed documents".into())
                    }
                    Expr::Word(word) => ("term", label(word)),
                    Expr::Phrase(phrase) => ("phrase", label(&format!("\"{phrase}\""))),
                    Expr::Near(left, right, distance, ordered) => (
                        "proximity",
                        label(&format!(
                            "{} {}/{distance} {}",
                            operand_label(left, self.fields),
                            if *ordered { "pre" } else { "w" },
                            operand_label(right, self.fields)
                        )),
                    ),
                    _ => unreachable!(),
                };
                (kind, text, matched)
            }
        };
        let mut ranges = Vec::new();
        let mut truncated = false;
        let mut matched_fields = Vec::new();
        if matched && children.is_empty() && kind != "all" {
            // Attribute unscoped clauses to the actual indexed fields, not
            // guessed occurrences in a body or translation supplied by the UI.
            for (target, _) in targets(self.fields, scope) {
                let query = compile(
                    expr,
                    self.fields,
                    self.searcher,
                    Some(target),
                    &mut ExpansionBudget::default(),
                )?;
                if self.matches(query.as_ref())? {
                    matched_fields.push(field_name(self.fields, target).into());
                }
            }
            if matched_fields.iter().any(|field| field == "body") {
                self.span_scan_truncated = false;
                let (positions, kind) = self.passage_spans(expr)?;
                for (start, end) in positions {
                    // A chunk can begin/end inside a source word. Reanalyzing
                    // that fragment must not invent a matching edge token.
                    if (!self.edges.start && self.tokens[start].offset_from == 0)
                        || (!self.edges.end && self.tokens[end].offset_to == self.text_bytes)
                    {
                        continue;
                    }
                    if ranges.len() == MAX_EVIDENCE_SPANS {
                        truncated = true;
                        break;
                    }
                    ranges.push(EvidenceSpan {
                        start: self.tokens[start].offset_from,
                        end: self.tokens[end].offset_to,
                        kind: kind.into(),
                        clause: text.clone(),
                    });
                }
                truncated |= self.span_scan_truncated;
            }
        }
        let field = match expr {
            Expr::Scope(field, _) => Some(field_name(self.fields, *field).into()),
            _ => scope.map(|field| field_name(self.fields, field).into()),
        };
        let clause = ExplainClause {
            kind: kind.into(),
            label: text,
            matched,
            field,
            matched_fields,
            children: children.iter().map(|child| child.clause.clone()).collect(),
        };
        Ok(Evaluated {
            clause,
            children,
            spans: ranges,
            spans_truncated: truncated,
        })
    }

    fn passage_spans(&mut self, expr: &Expr) -> Result<(Vec<(usize, usize)>, &'static str)> {
        match expr {
            Expr::Word(word) => {
                let pattern = WordPattern::parse(word)?;
                let (positions, kind) = match pattern {
                    WordPattern::Exact(tokens) => {
                        let terms: BTreeSet<_> = tokens.into_iter().map(|(_, text)| text).collect();
                        self.charge(self.tokens.len())?;
                        (
                            self.tokens
                                .iter()
                                .enumerate()
                                .filter(|(_, token)| terms.contains(&token.text))
                                .map(|(index, _)| (index, index))
                                .collect(),
                            "term",
                        )
                    }
                    pattern => (
                        self.pattern_spans(&pattern)?,
                        if matches!(pattern, WordPattern::Fuzzy(_, _)) {
                            "fuzzy"
                        } else {
                            "wildcard"
                        },
                    ),
                };
                Ok((positions, kind))
            }
            Expr::Phrase(text) => Ok((self.sequence_spans(&analyzed_tokens(text))?, "phrase")),
            Expr::Near(left, right, distance, ordered) => {
                let left = self.operand_spans(left)?;
                let right = self.operand_spans(right)?;
                let mut spans = self.near_spans(&left, &right, *distance)?;
                if !ordered {
                    spans.extend(self.near_spans(&right, &left, *distance)?);
                }
                spans.sort_unstable();
                spans.dedup();
                Ok((spans, "proximity"))
            }
            _ => unreachable!("only positive leaf clauses have passage ranges"),
        }
    }

    fn pattern_spans(&mut self, pattern: &WordPattern) -> Result<Vec<(usize, usize)>> {
        let source_work = self
            .tokens
            .iter()
            .map(|token| token.text.len().saturating_add(1))
            .sum::<usize>();
        let multiplier = match pattern {
            WordPattern::Wildcard(pattern) => pattern.len().saturating_add(1),
            _ => 1,
        };
        self.charge(source_work.saturating_mul(multiplier))?;
        static FUZZY_BUILDERS: std::sync::OnceLock<[LevenshteinAutomatonBuilder; 3]> =
            std::sync::OnceLock::new();
        let dfa: Option<DFA> = match pattern {
            WordPattern::Fuzzy(term, distance) => Some(
                FUZZY_BUILDERS.get_or_init(|| {
                    [
                        LevenshteinAutomatonBuilder::new(0, true),
                        LevenshteinAutomatonBuilder::new(1, true),
                        LevenshteinAutomatonBuilder::new(2, true),
                    ]
                })[*distance as usize]
                    .build_dfa(term),
            ),
            _ => None,
        };
        Ok(self.tokens.iter().enumerate().filter(|(_,token)| match pattern {
            WordPattern::Wildcard(pattern) => wildcard_matches(pattern,&token.text),
            WordPattern::Fuzzy(_,distance) => matches!(dfa.as_ref().unwrap().eval(token.text.as_bytes()), Distance::Exact(n) if n <= *distance),
            _ => unreachable!(),
        }).map(|(index,_)|(index,index)).collect())
    }

    fn sequence_spans(&mut self, sequence: &[(usize, String)]) -> Result<Vec<(usize, usize)>> {
        let Some((first_position, first)) = sequence.first() else {
            return Ok(Vec::new());
        };
        self.charge(self.tokens.len())?;
        let mut result = Vec::new();
        for start in 0..self.tokens.len() {
            if self.tokens[start].text != *first {
                continue;
            }
            self.charge(sequence.len())?;
            let mut end = start;
            let mut matches = true;
            for (offset, word) in sequence.iter().skip(1) {
                let position = self.tokens[start].position + offset - first_position;
                match self
                    .tokens
                    .binary_search_by_key(&position, |token| token.position)
                {
                    Ok(at) if self.tokens[at].text == *word => end = at,
                    _ => {
                        matches = false;
                        break;
                    }
                }
            }
            if matches {
                result.push((start, end));
            }
        }
        Ok(result)
    }

    fn operand_spans(&mut self, expr: &Expr) -> Result<Vec<(usize, usize)>> {
        match expr {
            Expr::Scope(field, inner) if *field == self.fields.body => self.operand_spans(inner),
            Expr::Scope(_, _) => Ok(Vec::new()),
            Expr::Or(parts) => {
                let mut spans = Vec::new();
                for part in parts {
                    spans.extend(self.operand_spans(part)?);
                }
                spans.sort_unstable();
                spans.dedup();
                Ok(spans)
            }
            Expr::Phrase(text) => self.sequence_spans(&analyzed_tokens(text)),
            Expr::Word(word) => match WordPattern::parse(word)? {
                WordPattern::Exact(tokens) => self.sequence_spans(&tokens),
                // The positional query uses this exact shared OSA expansion.
                WordPattern::Fuzzy(term, distance) => {
                    let source_work = self
                        .tokens
                        .iter()
                        .map(|token| token.text.len().saturating_add(1))
                        .sum::<usize>();
                    self.charge(source_work.saturating_mul(term.len().saturating_add(1)))?;
                    Ok(self
                        .tokens
                        .iter()
                        .enumerate()
                        .filter(|(_, token)| {
                            edit_distance(&term, &token.text, distance as usize)
                                <= distance as usize
                        })
                        .map(|(index, _)| (index, index))
                        .collect())
                }
                pattern => self.pattern_spans(&pattern),
            },
            _ => unreachable!("parser validates positional operands"),
        }
    }

    fn near_spans(
        &mut self,
        left: &[(usize, usize)],
        right: &[(usize, usize)],
        distance: u32,
    ) -> Result<Vec<(usize, usize)>> {
        let mut result = Vec::new();
        for (left_start, left_end) in left {
            self.charge(1)?;
            let end_position = self.tokens[*left_end].position;
            let next =
                right.partition_point(|(start, _)| self.tokens[*start].position <= end_position);
            for (right_start, right_end) in &right[next..] {
                self.charge(1)?;
                if self.tokens[*right_start].position - end_position - 1 > distance as usize {
                    break;
                }
                result.push((*left_start, *right_end));
                if result.len() > MAX_EVIDENCE_SPANS {
                    self.span_scan_truncated = true;
                    return Ok(result);
                }
            }
        }
        Ok(result)
    }
}
