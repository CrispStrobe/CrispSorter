use anyhow::Result;
use serde::{Deserialize, Serialize};
/// Tantivy full-text index — create, write, search.
///
/// Lives at `{data_dir}/fts/` alongside the LanceDB directory at `{data_dir}/lance/`.
///
/// Fields:
///   doc_id   — STRING STORED (links back to LanceDB row, used for delete/lookup)
///   owner_id — STRING STORED (multi-user filter)
///   headings — TEXT positional (boosted via should in query translator)
///   body     — TEXT positional (full document text)
///   language — STRING STORED (filter)
use std::path::Path;
use tantivy::{
    collector::TopDocs,
    query::{BooleanQuery, Occur, Query, TermQuery},
    schema::OwnedValue,
    schema::{
        Field, IndexRecordOption, Schema, SchemaBuilder, TextFieldIndexing, TextOptions, STORED,
        STRING,
    },
    Index, IndexReader, IndexWriter, ReloadPolicy, Score, TantivyDocument, Term,
};

use super::fts_query::{translate, SearchFields};
use super::schema::SearchFilters;

const WRITER_HEAP_MB: usize = 50;

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
    pub language: Field,
}

pub struct TantivyInput<'a> {
    pub doc_id: &'a str,
    pub owner_id: &'a str,
    pub language: &'a str,
    pub title: &'a str,
    pub headings: &'a str,
    pub body: &'a str,
}

impl FtsIndex {
    /// Open an existing Tantivy index at `dir`, or create it if absent.
    pub fn open_or_create(dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(dir)?;

        let (schema, fields) = build_schema();

        let mmap_dir = tantivy::directory::MmapDirectory::open(dir)?;
        let index = if Index::exists(&mmap_dir)? {
            Index::open_in_dir(dir)?
        } else {
            Index::create_in_dir(dir, schema)?
        };

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
    pub fn search_fields(&self) -> SearchFields {
        SearchFields {
            title: self.fields.title,
            headings: self.fields.headings,
            body: self.fields.body,
        }
    }

    pub fn writer(&self) -> Result<IndexWriter> {
        Ok(self.index.writer(WRITER_HEAP_MB * 1_024 * 1_024)?)
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
            .set_tokenizer("default")
            .set_index_option(IndexRecordOption::WithFreqsAndPositions),
    );

    let title = sb.add_text_field("title", text_positional.clone());
    let headings = sb.add_text_field("headings", text_positional.clone());
    let body = sb.add_text_field("body", text_positional);

    (
        sb.build(),
        IndexFields {
            doc_id,
            owner_id,
            title,
            headings,
            body,
            language,
        },
    )
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
}
