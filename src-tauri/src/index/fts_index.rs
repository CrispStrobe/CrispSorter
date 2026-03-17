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
use anyhow::Result;
use tantivy::{
    Index, IndexWriter, IndexReader,
    TantivyDocument,
    ReloadPolicy,
    schema::{
        Schema, SchemaBuilder, Field,
        STRING, STORED,
        IndexRecordOption, TextOptions, TextFieldIndexing,
    },
    query::{BooleanQuery, Occur, TermQuery, Query},
    collector::TopDocs,
    Term,
    Score,
    schema::OwnedValue,
};
use serde::{Deserialize, Serialize};

use super::fts_query::{translate, SearchFields};
use super::schema::SearchFilters;

const WRITER_HEAP_MB: usize = 50;

pub struct FtsIndex {
    pub index: Index,
    pub fields: IndexFields,
    reader: IndexReader,
}

pub struct IndexFields {
    pub doc_id:   Field,
    pub owner_id: Field,
    pub title:    Field,
    pub headings: Field,
    pub body:     Field,
    pub language: Field,
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

        Ok(FtsIndex { index, fields, reader })
    }

    /// Return `SearchFields` for the dtSearch query translator.
    pub fn search_fields(&self) -> SearchFields {
        SearchFields {
            title:    self.fields.title,
            headings: self.fields.headings,
            body:     self.fields.body,
        }
    }

    pub fn writer(&self) -> Result<IndexWriter> {
        Ok(self.index.writer(WRITER_HEAP_MB * 1_024 * 1_024)?)
    }

    /// Add a document to an open writer. Call `writer.commit()` when done.
    pub fn add_document(
        &self,
        writer: &mut IndexWriter,
        doc_id: &str,
        owner_id: &str,
        language: &str,
        title: &str,
        headings: &str,
        body: &str,
    ) -> Result<()> {
        let mut doc = TantivyDocument::default();
        doc.add_text(self.fields.doc_id,   doc_id);
        doc.add_text(self.fields.owner_id, owner_id);
        doc.add_text(self.fields.language, language);
        doc.add_text(self.fields.title,    title);
        doc.add_text(self.fields.headings, headings);
        doc.add_text(self.fields.body,     body);
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
        if let OwnedValue::Str(s) = v { Some(s.clone()) } else { None }
    })
}

// ── Schema builder ─────────────────────────────────────────────────────────

fn build_schema() -> (Schema, IndexFields) {
    let mut sb = SchemaBuilder::new();

    let doc_id   = sb.add_text_field("doc_id",   STRING | STORED);
    let owner_id = sb.add_text_field("owner_id", STRING | STORED);
    let language = sb.add_text_field("language", STRING | STORED);

    let text_positional = TextOptions::default().set_indexing_options(
        TextFieldIndexing::default()
            .set_tokenizer("default")
            .set_index_option(IndexRecordOption::WithFreqsAndPositions),
    );

    let title    = sb.add_text_field("title",    text_positional.clone());
    let headings = sb.add_text_field("headings", text_positional.clone());
    let body     = sb.add_text_field("body",     text_positional);

    (sb.build(), IndexFields { doc_id, owner_id, title, headings, body, language })
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
        idx.add_document(&mut w, "doc1", "user1", "en", "Introduction", "", "The theology of Karl Rahner explores grace.").unwrap();
        idx.add_document(&mut w, "doc2", "user1", "de", "Einleitung", "", "Karl Barth und die Gnadenlehre der Kirche.").unwrap();
        w.commit().unwrap();

        let hits = idx.search("rahner", &SearchFilters::default(), 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].doc_id, "doc1");
    }

    #[test]
    fn owner_filter() {
        let (idx, _dir) = make_index();
        let mut w = idx.writer().unwrap();
        idx.add_document(&mut w, "d1", "user1", "en", "", "", "grace theology rahner").unwrap();
        idx.add_document(&mut w, "d2", "user2", "en", "", "", "grace theology barth").unwrap();
        w.commit().unwrap();

        let f = SearchFilters { owner_id: Some("user1".to_owned()), ..Default::default() };
        let hits = idx.search("grace", &f, 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].doc_id, "d1");
    }

    #[test]
    fn delete_document() {
        let (idx, _dir) = make_index();
        {
            let mut w = idx.writer().unwrap();
            idx.add_document(&mut w, "d1", "u1", "en", "", "", "rahner anonymous theology").unwrap();
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
        idx.add_document(&mut w, "d1", "u1", "en", "", "",
            "anonymity anonymous anonymously").unwrap();
        w.commit().unwrap();
        let hits = idx.search("anon*", &SearchFilters::default(), 10).unwrap();
        assert!(!hits.is_empty());
    }

    #[test]
    fn proximity_w50() {
        let (idx, _dir) = make_index();
        let mut w = idx.writer().unwrap();
        // "rahner" and "anonymous" are 3 words apart
        idx.add_document(&mut w, "d1", "u1", "en", "", "",
            "rahner writes about the anonymous christian concept").unwrap();
        // Control: rahner appears but anonymous does not
        idx.add_document(&mut w, "d2", "u1", "en", "", "", "rahner wrote about grace").unwrap();
        w.commit().unwrap();

        let hits = idx.search("rahner w/50 anonymous", &SearchFilters::default(), 10).unwrap();
        let ids: Vec<_> = hits.iter().map(|h| h.doc_id.as_str()).collect();
        assert!(ids.contains(&"d1"), "d1 should match w/50 query");
    }

    #[test]
    fn title_boosting() {
        let (idx, _dir) = make_index();
        let mut w = idx.writer().unwrap();
        // d1 has "Recht" in title
        idx.add_document(&mut w, "d1", "u1", "de", "Recht unter Druck", "", "Abstract text...").unwrap();
        // d2 has "Recht" only in body
        idx.add_document(&mut w, "d2", "u1", "de", "Other Title", "", "This document mentions Recht once.").unwrap();
        w.commit().unwrap();

        let hits = idx.search("Recht", &SearchFilters::default(), 10).unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].doc_id, "d1", "Document with 'Recht' in title should outrank document with 'Recht' in body");
    }
}
