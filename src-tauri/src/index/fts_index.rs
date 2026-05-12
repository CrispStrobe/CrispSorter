use anyhow::Result;
use serde::{Deserialize, Serialize};
/// Tantivy full-text index — create, write, search.
///
/// Lives at `{data_dir}/fts/` alongside the LanceDB directory at `{data_dir}/lance/`.
///
/// Fields:
///   doc_id          — STRING STORED (links back to LanceDB row, used for delete/lookup)
///   owner_id        — STRING STORED (multi-user filter)
///   headings        — TEXT positional (boosted via should in query translator)
///   body            — TEXT positional (full document text)
///   body_translated — TEXT positional, indexed-only (RawDocument.translated_text
///                     when the extractor ran a translation pass; closes the
///                     "English query against a Bosnian doc with English
///                     translation doesn't hit BM25" gap from PLAN.md).
///                     Indexes lazily — legacy on-disk schemas without this
///                     field still open fine; `IndexFields.body_translated`
///                     becomes `None` and the field is skipped for both
///                     ingest and search until the user rebuilds.
///   language        — STRING STORED (filter)
use std::path::Path;
use tantivy::{
    collector::TopDocs,
    query::{BooleanQuery, Occur, Query, TermQuery},
    schema::OwnedValue,
    schema::{
        Field, IndexRecordOption, Schema, SchemaBuilder, TextFieldIndexing, TextOptions, STORED,
        STRING,
    },
    tokenizer::{
        AsciiFoldingFilter, LowerCaser, RemoveLongFilter, SimpleTokenizer, TextAnalyzer,
    },
    Index, IndexReader, IndexWriter, ReloadPolicy, Score, TantivyDocument, Term,
};

use super::fts_query::{translate, SearchFields};
use super::schema::SearchFilters;

const WRITER_HEAP_MB: usize = 50;

/// Tokenizer used for `title`/`headings`/`body`. Mirrors the query-side
/// `fold_accents` so a query for `München` matches an indexed `München` and
/// vice-versa for `Munchen`. Built from Tantivy's stock filters:
/// SimpleTokenizer + RemoveLong(40) + LowerCaser + AsciiFoldingFilter.
const ASCII_FOLD_TOKENIZER: &str = "ascii_folding";

fn register_tokenizers(index: &Index) {
    let analyzer = TextAnalyzer::builder(SimpleTokenizer::default())
        .filter(RemoveLongFilter::limit(40))
        .filter(LowerCaser)
        .filter(AsciiFoldingFilter)
        .build();
    index
        .tokenizers()
        .register(ASCII_FOLD_TOKENIZER, analyzer);
}

pub struct FtsIndex {
    pub index: Index,
    pub fields: IndexFields,
    reader: IndexReader,
}

pub struct IndexFields {
    pub doc_id: Field,
    pub owner_id: Field,
    pub title: Field,
    pub headings: Field,
    pub body: Field,
    /// `Some` for indexes created on or after the body_translated schema
    /// rev; `None` for legacy on-disk indexes whose schema predates it
    /// (open path detects this and skips the field for read+write).
    pub body_translated: Option<Field>,
    pub language: Field,
}

pub struct TantivyInput<'a> {
    pub doc_id: &'a str,
    pub owner_id: &'a str,
    pub language: &'a str,
    pub title: &'a str,
    pub headings: &'a str,
    pub body: &'a str,
    /// MT-pass output (`RawDocument.translated_text`) — only written
    /// when both this value is `Some(_)` *and* `IndexFields.body_translated`
    /// is `Some(_)` (i.e. the on-disk schema has the field).  Pass `None`
    /// for legacy ingest paths and L1 manifest ingest where no
    /// translation runs.
    pub body_translated: Option<&'a str>,
}

impl FtsIndex {
    /// Open an existing Tantivy index at `dir`, or create it if absent.
    /// Fresh indexes get the full schema including `body_translated`;
    /// existing indexes are opened as-is, and `IndexFields.body_translated`
    /// is `None` if the on-disk schema predates that field (the field
    /// can't be retroactively added to existing Tantivy segments without
    /// a full rebuild — a future migration will handle that).
    pub fn open_or_create(dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(dir)?;

        let (schema, fresh_fields) = build_schema();

        let mmap_dir = tantivy::directory::MmapDirectory::open(dir)?;
        let (index, fields) = if Index::exists(&mmap_dir)? {
            let index = Index::open_in_dir(dir)?;
            let fields = bind_fields_from_disk(&index)?;
            (index, fields)
        } else {
            let index = Index::create_in_dir(dir, schema)?;
            (index, fresh_fields)
        };

        register_tokenizers(&index);

        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::OnCommitWithDelay)
            .try_into()?;

        Ok(FtsIndex {
            index,
            fields,
            reader,
        })
    }

    /// Return `SearchFields` for the dtSearch query translator.
    /// `body_translated` is only populated when the on-disk schema has
    /// the field; legacy indexes return `None` and the translator
    /// silently skips the disjunction.
    pub fn search_fields(&self) -> SearchFields {
        SearchFields {
            title: self.fields.title,
            headings: self.fields.headings,
            body: self.fields.body,
            body_translated: self.fields.body_translated,
        }
    }

    pub fn writer(&self) -> Result<IndexWriter> {
        Ok(self.index.writer(WRITER_HEAP_MB * 1_024 * 1_024)?)
    }

    /// Total number of documents (across all segments) in the Tantivy index.
    pub fn doc_count(&self) -> u64 {
        let _ = self.reader.reload();
        self.reader.searcher().num_docs()
    }

    /// Add a document to an open writer. Call `writer.commit()` when done.
    pub fn add_document(&self, writer: &mut IndexWriter, input: TantivyInput) -> Result<()> {
        let mut doc = TantivyDocument::default();
        doc.add_text(self.fields.doc_id, input.doc_id);
        doc.add_text(self.fields.owner_id, input.owner_id);
        doc.add_text(self.fields.language, input.language);
        doc.add_text(self.fields.title, input.title);
        doc.add_text(self.fields.headings, input.headings);
        doc.add_text(self.fields.body, input.body);

        // body_translated is only written when both the on-disk schema
        // exposes the field AND the caller supplied a translation.  Legacy
        // schemas: silently skip.  No translation: ditto.
        if let (Some(field), Some(text)) = (self.fields.body_translated, input.body_translated) {
            if !text.is_empty() {
                doc.add_text(field, text);
            }
        }

        writer.add_document(doc)?;
        Ok(())
    }

    /// Delete all index entries for a `doc_id`. Call `writer.commit()` after.
    pub fn delete_document(&self, writer: &mut IndexWriter, doc_id: &str) -> Result<()> {
        writer.delete_term(Term::from_field_text(self.fields.doc_id, doc_id));
        Ok(())
    }

    /// Full-text search with dtSearch-style query syntax.
    /// Optionally pre-filters by `owner_id` from `filters`.
    pub fn search(
        &self,
        query_str: &str,
        filters: &SearchFilters,
        limit: usize,
    ) -> Result<Vec<FtsHit>> {
        self.reader.reload()?;
        let searcher = self.reader.searcher();
        let sf = self.search_fields();

        let query = translate(query_str, &self.reader, &sf)?;

        // Wrap with owner_id filter if provided.
        let effective_query: Box<dyn Query> = if let Some(ref oid) = filters.owner_id {
            let owner_q: Box<dyn Query> = Box::new(TermQuery::new(
                Term::from_field_text(self.fields.owner_id, oid),
                IndexRecordOption::Basic,
            ));
            Box::new(BooleanQuery::new(vec![
                (Occur::Must, query),
                (Occur::Must, owner_q),
            ]))
        } else {
            query
        };

        let top_docs = searcher.search(&effective_query, &TopDocs::with_limit(limit))?;

        let mut hits = Vec::with_capacity(top_docs.len());
        for (score, doc_addr) in top_docs {
            let doc: TantivyDocument = searcher.doc(doc_addr)?;
            let doc_id = owned_str(&doc, self.fields.doc_id).unwrap_or_default();
            hits.push(FtsHit { doc_id, score });
        }
        Ok(hits)
    }
}

/// A single full-text search hit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FtsHit {
    pub doc_id: String,
    pub score: Score,
}

// ── Helpers ────────────────────────────────────────────────────────────────

/// Extract a stored `Str` value from a `TantivyDocument`.
fn owned_str(doc: &TantivyDocument, field: Field) -> Option<String> {
    doc.get_first(field).and_then(|v| {
        if let OwnedValue::Str(s) = v {
            Some(s.clone())
        } else {
            None
        }
    })
}

// ── Schema builder ─────────────────────────────────────────────────────────

fn build_schema() -> (Schema, IndexFields) {
    let mut sb = SchemaBuilder::new();

    let doc_id = sb.add_text_field("doc_id", STRING | STORED);
    let owner_id = sb.add_text_field("owner_id", STRING | STORED);
    let language = sb.add_text_field("language", STRING | STORED);

    let text_positional = TextOptions::default().set_indexing_options(
        TextFieldIndexing::default()
            .set_tokenizer(ASCII_FOLD_TOKENIZER)
            .set_index_option(IndexRecordOption::WithFreqsAndPositions),
    );

    let title = sb.add_text_field("title", text_positional.clone());
    let headings = sb.add_text_field("headings", text_positional.clone());
    let body = sb.add_text_field("body", text_positional.clone());
    // Same tokenizer as `body` so a query "hello" matches the translated
    // column the same way it would the original.  STORED is intentionally
    // omitted — we never need the translated text back from Tantivy
    // (snippets come from LanceDB's `text_translated` column).
    let body_translated = sb.add_text_field("body_translated", text_positional);

    (
        sb.build(),
        IndexFields {
            doc_id,
            owner_id,
            title,
            headings,
            body,
            body_translated: Some(body_translated),
            language,
        },
    )
}

/// Bind `IndexFields` from an already-opened Tantivy index.  `body_translated`
/// becomes `None` if the on-disk schema predates that field — legacy
/// indexes opened by older CrispSorter builds.  All other fields are
/// required and an error is returned if any is missing (which would
/// mean an index from a different application).
fn bind_fields_from_disk(index: &Index) -> Result<IndexFields> {
    let schema = index.schema();
    let required = |name: &str| -> Result<Field> {
        schema
            .get_field(name)
            .map_err(|_| anyhow::anyhow!("FTS schema on disk is missing required field `{name}` — was the directory created by a different application?"))
    };
    Ok(IndexFields {
        doc_id: required("doc_id")?,
        owner_id: required("owner_id")?,
        title: required("title")?,
        headings: required("headings")?,
        body: required("body")?,
        // Optional: legacy indexes don't have it.  `Schema::get_field`
        // returns `Err(FieldNotFound)` rather than a typed Option, so
        // collapse that into None for the read-only check.
        body_translated: schema.get_field("body_translated").ok(),
        language: required("language")?,
    })
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_index() -> (FtsIndex, TempDir) {
        let dir = TempDir::new().unwrap();
        let idx = FtsIndex::open_or_create(dir.path()).unwrap();
        (idx, dir)
    }

    #[test]
    fn create_and_reopen() {
        let dir = TempDir::new().unwrap();
        drop(FtsIndex::open_or_create(dir.path()).unwrap());
        FtsIndex::open_or_create(dir.path()).unwrap();
    }

    #[test]
    fn write_and_search_basic() {
        let (idx, _dir) = make_index();
        let mut w = idx.writer().unwrap();
        idx.add_document(
            &mut w,
            TantivyInput {
                doc_id: "doc1",
                owner_id: "user1",
                language: "en",
                title: "Introduction",
                headings: "",
                body: "The theology of Karl Rahner explores grace.",
                body_translated: None,
            },
        )
        .unwrap();
        idx.add_document(
            &mut w,
            TantivyInput {
                doc_id: "doc2",
                owner_id: "user1",
                language: "de",
                title: "Einleitung",
                headings: "",
                body: "Karl Barth und die Gnadenlehre der Kirche.",
                body_translated: None,
            },
        )
        .unwrap();
        w.commit().unwrap();

        let hits = idx.search("rahner", &SearchFilters::default(), 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].doc_id, "doc1");
    }

    #[test]
    fn owner_filter() {
        let (idx, _dir) = make_index();
        let mut w = idx.writer().unwrap();
        idx.add_document(
            &mut w,
            TantivyInput {
                doc_id: "d1",
                owner_id: "user1",
                language: "en",
                title: "",
                headings: "",
                body: "grace theology rahner",
                body_translated: None,
            },
        )
        .unwrap();
        idx.add_document(
            &mut w,
            TantivyInput {
                doc_id: "d2",
                owner_id: "user2",
                language: "en",
                title: "",
                headings: "",
                body: "grace theology barth",
                body_translated: None,
            },
        )
        .unwrap();
        w.commit().unwrap();

        let f = SearchFilters {
            owner_id: Some("user1".to_owned()),
            ..Default::default()
        };
        let hits = idx.search("grace", &f, 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].doc_id, "d1");
    }

    #[test]
    fn delete_document() {
        let (idx, _dir) = make_index();
        {
            let mut w = idx.writer().unwrap();
            idx.add_document(
                &mut w,
                TantivyInput {
                    doc_id: "d1",
                    owner_id: "u1",
                    language: "en",
                    title: "",
                    headings: "",
                    body: "rahner anonymous theology",
                    body_translated: None,
                },
            )
            .unwrap();
            w.commit().unwrap();
        } // drop writer to release the lockfile before creating a second one

        let mut w2 = idx.writer().unwrap();
        idx.delete_document(&mut w2, "d1").unwrap();
        w2.commit().unwrap();

        let hits = idx.search("rahner", &SearchFilters::default(), 10).unwrap();
        assert!(hits.is_empty());
    }

    #[test]
    fn wildcard_search() {
        let (idx, _dir) = make_index();
        let mut w = idx.writer().unwrap();
        idx.add_document(
            &mut w,
            TantivyInput {
                doc_id: "d1",
                owner_id: "u1",
                language: "en",
                title: "",
                headings: "",
                body: "anonymity anonymous anonymously",
                body_translated: None,
            },
        )
        .unwrap();
        w.commit().unwrap();
        let hits = idx.search("anon*", &SearchFilters::default(), 10).unwrap();
        assert!(!hits.is_empty());
    }

    #[test]
    fn proximity_w50() {
        let (idx, _dir) = make_index();
        let mut w = idx.writer().unwrap();
        // "rahner" and "anonymous" are 3 words apart
        idx.add_document(
            &mut w,
            TantivyInput {
                doc_id: "d1",
                owner_id: "u1",
                language: "en",
                title: "",
                headings: "",
                body: "rahner writes about the anonymous christian concept",
                body_translated: None,
            },
        )
        .unwrap();
        // Control: rahner appears but anonymous does not
        idx.add_document(
            &mut w,
            TantivyInput {
                doc_id: "d2",
                owner_id: "u1",
                language: "en",
                title: "",
                headings: "",
                body: "rahner wrote about grace",
                body_translated: None,
            },
        )
        .unwrap();
        w.commit().unwrap();

        let hits = idx
            .search("rahner w/50 anonymous", &SearchFilters::default(), 10)
            .unwrap();
        let ids: Vec<_> = hits.iter().map(|h| h.doc_id.as_str()).collect();
        assert!(ids.contains(&"d1"), "d1 should match w/50 query");
    }

    #[test]
    fn title_boosting() {
        let (idx, _dir) = make_index();
        let mut w = idx.writer().unwrap();
        // d1 has "Recht" in title
        idx.add_document(
            &mut w,
            TantivyInput {
                doc_id: "d1",
                owner_id: "u1",
                language: "de",
                title: "Recht unter Druck",
                headings: "",
                body: "Abstract text...",
                body_translated: None,
            },
        )
        .unwrap();
        // d2 has "Recht" only in body
        idx.add_document(
            &mut w,
            TantivyInput {
                doc_id: "d2",
                owner_id: "u1",
                language: "de",
                title: "Other Title",
                headings: "",
                body: "This document mentions Recht once.",
                body_translated: None,
            },
        )
        .unwrap();
        w.commit().unwrap();

        let hits = idx.search("Recht", &SearchFilters::default(), 10).unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(
            hits[0].doc_id, "d1",
            "Document with 'Recht' in title should outrank document with 'Recht' in body"
        );
    }

    #[test]
    fn scenario_recht_ranking_fixed() {
        let (idx, _dir) = make_index();
        let mut w = idx.writer().unwrap();

        // 1. Constitutional AI (Bai 2022) — mentions Recht many times in body, but not in title.
        idx.add_document(
            &mut w,
            TantivyInput {
                doc_id: "constitutional-ai",
                owner_id: "u1",
                language: "en",
                title: "Constitutional AI: Harmlessness from AI Feedback",
                headings: "Intro",
                body: "Recht and safety. The framework of Recht is based on AI feedback. The Recht principles are key. Recht, Recht, Recht, Recht, Recht.",
                body_translated: None,
            },
        )
        .unwrap();

        // 2. Recht unter Druck (Abstract) — Title HAS 'Recht'.
        idx.add_document(
            &mut w,
            TantivyInput {
                doc_id: "recht-unter-druck",
                owner_id: "u1",
                language: "de",
                title: "Recht unter Druck_Abstract.docx",
                headings: "Workshop",
                body: "Während Angriffe auf den Rechtsstaat in Europa...",
                body_translated: None,
            },
        )
        .unwrap();

        // 3. Akzente (Bistum Essen) — mentions Recht occasionally in body.
        idx.add_document(
            &mut w,
            TantivyInput {
                doc_id: "akzente",
                owner_id: "u1",
                language: "de",
                title: "2503329_BistumEssen_Akzente63_01-2026.pdf",
                headings: "Dialog",
                body: "Die Rolle Deutschlands in einer Weltordnung. Es geht um Recht und Macht.",
                body_translated: None,
            },
        )
        .unwrap();

        w.commit().unwrap();

        // Query for 'Recht'
        let hits = idx.search("Recht", &SearchFilters::default(), 10).unwrap();

        assert!(!hits.is_empty());

        // BEFORE THE FIX: 'constitutional-ai' would rank #1 because it has more body mentions.
        // AFTER THE FIX: 'recht-unter-druck' should rank #1 due to Title Boosting (3x).
        assert_eq!(
            hits[0].doc_id, "recht-unter-druck",
            "Document with 'Recht' in title should rank first despite fewer body matches"
        );

        // FTS score for the title match should be much higher than the body matches in Constitutional AI
        println!("Hit 0 (Title): {} score {}", hits[0].doc_id, hits[0].score);
        println!("Hit 1 (Body ): {} score {}", hits[1].doc_id, hits[1].score);
    }

    #[test]
    fn scenario_academic_integration() {
        let (idx, _dir) = make_index();

        let doc_id = "academic-doc-2019";
        let title = "Integration – Dialog – Integrationsdialog? Zeithistorisch akzentuierte Perspektiven auf sozialintegrative Potentiale des christlich-islamischen Dialogs";
        let _author = "Academic Author";

        // Simulating 3 major sections.
        let full_text = "
            Begriffsarbeit. Den christlich-islamischen Dialog fasse ich als intentionale Begegnungen von Christen und Muslimen auf. 
            Der Soziologe Wilhelm Heitmeyer hat drei grundlegende Dimensionen der Sozialintegration entwickelt: kulturell-expressiver Sozialintegration, kommunikativ-interaktive Sozialintegration und funktionale Systemintegration.
            Die integrationsrelevante Innendimension. In der evangelischen St. Reinoldi-Kirche trafen sich 1970 Christen und Muslime zu einer Gebetsandacht.
            Die integrationsrelevante Außendimension. Moscheebauten und Ezan-Ruf in Marl, Gelsenkirchen und Witten.
            Dialog im Zugriff der Politik. Samuel Huntington formulierte 'Clash of Civilizations'. Die Ahmadiyya-Gemeinde in Münster.
            Abwägendes Resümee. Der christlich-islamische Dialog weist Ambivalenzen auf.
        ";

        {
            let mut w = idx.writer().unwrap();
            idx.add_document(
                &mut w,
                TantivyInput {
                    doc_id,
                    owner_id: "u1",
                    language: "de",
                    title,
                    headings: "Begriffsarbeit Innendimension Außendimension Resümee",
                    body: full_text,
                    body_translated: None,
                },
            )
            .unwrap();
            w.commit().unwrap();
        }

        let f = SearchFilters::default();

        // 1. Search for section header
        let hits = idx.search("Begriffsarbeit", &f, 10).unwrap();
        assert_eq!(
            hits[0].doc_id, doc_id,
            "Should find document via section header 'Begriffsarbeit'"
        );

        // 2. Search for specific author + concept
        let hits = idx.search("Heitmeyer Sozialintegration", &f, 10).unwrap();
        assert_eq!(
            hits[0].doc_id, doc_id,
            "Should find document via 'Heitmeyer Sozialintegration'"
        );

        // 3. Search for specific location
        let hits = idx.search("Reinoldi", &f, 10).unwrap();
        assert_eq!(
            hits[0].doc_id, doc_id,
            "Should find document via 'Reinoldi'"
        );

        // 4. Search for famous concept
        let hits = idx.search("\"Clash of Civilizations\"", &f, 10).unwrap();
        assert_eq!(
            hits[0].doc_id, doc_id,
            "Should find document via phrase 'Clash of Civilizations'"
        );

        // 5. Search for specific group
        let hits = idx.search("Ahmadiyya", &f, 10).unwrap();
        assert_eq!(
            hits[0].doc_id, doc_id,
            "Should find document via 'Ahmadiyya'"
        );

        // 6. Search for generic term
        let hits = idx.search("Mustermann", &f, 10).unwrap();
        assert!(hits.is_empty(), "Generic name is NOT currently indexed");

        // Re-index with name in title to check if that works
        {
            let mut w2 = idx.writer().unwrap();
            idx.add_document(
                &mut w2,
                TantivyInput {
                    doc_id: "doc-v2",
                    owner_id: "u1",
                    language: "de",
                    title: "Erika Mustermann: Integration",
                    headings: "",
                    body: "text",
                    body_translated: None,
                },
            )
            .unwrap();
            w2.commit().unwrap();
        }
        let hits = idx.search("Mustermann", &f, 10).unwrap();
        assert_eq!(
            hits[0].doc_id, "doc-v2",
            "Should find document if name is in indexed title"
        );
    }

    #[test]
    fn scenario_accent_folding() {
        let (idx, _dir) = make_index();
        {
            let mut w = idx.writer().unwrap();
            idx.add_document(
                &mut w,
                TantivyInput {
                    doc_id: "d1",
                    owner_id: "u1",
                    language: "de",
                    title: "München",
                    headings: "",
                    body: "text content",
                    body_translated: None,
                },
            )
            .unwrap();
            w.commit().unwrap();
        }

        let f = SearchFilters::default();
        // Search with 'ü'
        let hits = idx.search("München", &f, 10).unwrap();
        assert_eq!(hits.len(), 1, "Should find with exact umlaut");

        // Search with 'u' (folding)
        let hits2 = idx.search("Munchen", &f, 10).unwrap();
        assert_eq!(hits2.len(), 1, "Should find with folded 'u' instead of 'ü'");
    }

    #[test]
    fn scenario_wildcards_allowed() {
        let (idx, _dir) = make_index();
        {
            let mut w = idx.writer().unwrap();
            idx.add_document(
                &mut w,
                TantivyInput {
                    doc_id: "d1",
                    owner_id: "u1",
                    language: "de",
                    title: "Integration und Dialog",
                    headings: "",
                    body: "text content",
                    body_translated: None,
                },
            )
            .unwrap();
            w.commit().unwrap();
        }

        let f = SearchFilters::default();

        // Suffix wildcard
        let hits = idx.search("Integ*", &f, 10).unwrap();
        assert_eq!(hits.len(), 1, "Suffix wildcard Integ* should work");

        // Mid wildcard
        let hits2 = idx.search("Inte?ration", &f, 10).unwrap();
        assert_eq!(hits2.len(), 1, "Middle wildcard Inte?ration should work");

        // Leading wildcard
        let hits3 = idx.search("*tegration", &f, 10).unwrap();
        assert_eq!(hits3.len(), 1, "Leading wildcard *tegration should work");
    }

    /// Fresh schemas have body_translated; the FTS picks up an English
    /// query against a Bosnian original whose English MT-pass output
    /// is in the body_translated field.  Pins the FTS-over-translated-
    /// body P13.5 follow-up: without it, the Bosnian doc would lose
    /// BM25 scoring entirely on an English query.
    #[test]
    fn body_translated_makes_translated_text_searchable() {
        let (idx, _dir) = make_index();
        assert!(
            idx.fields.body_translated.is_some(),
            "fresh schema must include body_translated"
        );

        let mut w = idx.writer().unwrap();
        // Bosnian original + English translation — the canonical
        // cross-language case from the P13.5 follow-up motivation.
        idx.add_document(
            &mut w,
            TantivyInput {
                doc_id: "bs1",
                owner_id: "u1",
                language: "bs",
                title: "Pozdrav",
                headings: "",
                body: "Zdravo, kako si danas?",
                body_translated: Some("Hello, how are you today?"),
            },
        )
        .unwrap();
        // Pure-Bosnian doc with no translation: the query word "hello"
        // shouldn't reach it.
        idx.add_document(
            &mut w,
            TantivyInput {
                doc_id: "bs2",
                owner_id: "u1",
                language: "bs",
                title: "Drugi tekst",
                headings: "",
                body: "Tekst koji ne sadrži ništa korisno.",
                body_translated: None,
            },
        )
        .unwrap();
        w.commit().unwrap();

        let f = SearchFilters::default();
        let hits = idx.search("hello", &f, 10).unwrap();
        assert_eq!(hits.len(), 1, "English query should hit only the translated doc");
        assert_eq!(hits[0].doc_id, "bs1");
    }

    /// `bind_fields_from_disk` makes body_translated `None` when the
    /// on-disk schema predates the field.  Simulating an "old schema"
    /// in-process is awkward (Tantivy create+open both come from
    /// `build_schema`); instead exercise the open path with a
    /// purpose-built schema that intentionally omits body_translated
    /// and confirm we fail loudly when a *required* field is missing
    /// but succeed-with-None when only the optional one is.
    #[test]
    fn bind_fields_from_disk_handles_legacy_schema() {
        use tantivy::schema::{SchemaBuilder, STORED, STRING};
        let dir = TempDir::new().unwrap();
        // Build a schema with all the legacy required fields but
        // intentionally without body_translated.
        let mut sb = SchemaBuilder::new();
        sb.add_text_field("doc_id", STRING | STORED);
        sb.add_text_field("owner_id", STRING | STORED);
        sb.add_text_field("language", STRING | STORED);
        let text_positional = TextOptions::default().set_indexing_options(
            TextFieldIndexing::default()
                .set_tokenizer(ASCII_FOLD_TOKENIZER)
                .set_index_option(IndexRecordOption::WithFreqsAndPositions),
        );
        sb.add_text_field("title", text_positional.clone());
        sb.add_text_field("headings", text_positional.clone());
        sb.add_text_field("body", text_positional);
        let legacy_schema = sb.build();
        let legacy_idx = Index::create_in_dir(dir.path(), legacy_schema).unwrap();
        drop(legacy_idx);

        // Re-open through the normal path — bind_fields_from_disk
        // should find all required fields and leave body_translated
        // as None.
        let fts = FtsIndex::open_or_create(dir.path()).unwrap();
        assert!(
            fts.fields.body_translated.is_none(),
            "legacy schema must surface body_translated as None"
        );

        // Adding a doc with body_translated: Some(_) must still
        // succeed (the write silently skips the field when fields
        // hasn't got it) — pins the graceful-degrade write path.
        let mut w = fts.writer().unwrap();
        fts.add_document(
            &mut w,
            TantivyInput {
                doc_id: "legacy1",
                owner_id: "u1",
                language: "bs",
                title: "",
                headings: "",
                body: "Tekst",
                body_translated: Some("Text"),
            },
        )
        .expect("add_document on legacy schema must succeed");
        w.commit().unwrap();

        // And the query path: "text" shouldn't find anything on the
        // legacy schema (no body_translated field).
        let hits = fts.search("text", &SearchFilters::default(), 10).unwrap();
        assert!(
            hits.is_empty(),
            "legacy schema must skip body_translated in search, not panic"
        );
    }
}
