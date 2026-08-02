# Working with CrispSorter's AI — what it does, and how it fails

Article 4 of Regulation (EU) 2024/1689 has required, since 2 February 2025, that
providers and deployers ensure a sufficient level of AI literacy among the people
operating their systems. It is an organisational duty, not a feature — no code
change discharges it. What a provider *can* do is stop each deployer from having
to work this out for themselves, which is what this page is.

Read it if you run CrispSorter over documents that matter, or if you are
responsible for people who do. It assumes no machine-learning background.

**Related:** [`ai-act.md`](ai-act.md) is the regulatory inventory — every AI
capability and its classification. This page is the operational counterpart:
what to expect from the output.

---

## The one thing that matters

**Everything the models produce is a suggestion, and suggestions are sometimes
confidently wrong.**

Not "occasionally low-confidence". Wrong, in a well-formed, plausible-looking
way, with no outward sign that this answer is different from the correct ones
either side of it. A model that extracts the right author for nine files will
extract a wrong one for the tenth in exactly the same format and tone.

This is why the app puts you at the decision point rather than acting: batch
suggestions land in a review grid and **files move only when you confirm**. That
design is doing real work. If you find yourself confirming without looking, you
have removed the safeguard the system was built around.

---

## Capability by capability

### Metadata extraction (title, author, year → folder placement)

**What it does.** Reads the first pages, asks a language model to name the
title, author and year, and computes a destination path from the answer.

**How it fails.**
- Picks the *publisher*, series editor or a cited author over the actual author.
- Reads the year off a reprint notice, a copyright line, or the date the PDF was
  scanned.
- Takes a running header, a chapter title or the cover subtitle as the title.
- Invents plausible values for a document that contains none — a scanned form, a
  cover sheet, an exhibit. This is the failure mode with the fewest outward
  signs.
- Degrades sharply when OCR quality is poor: a garbled first page yields
  confident nonsense rather than an error.

**How to work with it.** Sort a small sample first and check the grid against
the documents. Pay attention to the year column — it is the field that is wrong
most often and the one that is hardest to spot afterwards, because a file in the
wrong year folder still looks correctly filed.

### OCR (scanned pages → text)

**What it does.** Turns pixels into characters. It renders what is on the page;
it does not generate content.

**How it fails.** Confuses visually similar characters (`rn`/`m`, `1`/`l`/`I`,
`0`/`O`); mangles tables and multi-column layouts by reading across columns;
drops handwriting and low-contrast stamps; silently produces a plausible word
where the scan was unreadable.

**How to work with it.** Never treat OCR output as a faithful copy for anything
consequential — figures, names, dates, legal text. Check against the image.
Numbers are the highest-risk content because a wrong digit reads as valid.

### Transcription (audio → text)

**What it does.** Renders speech as text. Like OCR, it is a rendering, not a
generation — which is why the AI Act's synthetic-content marking duty does not
attach to it.

**How it fails.** Guesses at proper nouns, technical vocabulary and acronyms;
loses or misattributes speakers when they overlap; degrades badly with
background noise, accents outside the training distribution, and quiet
speakers. It generally produces *fluent* text regardless — a confident wrong
name reads exactly like a correct one.

### Translation

**What it does.** Machine translation of extracted text or a whole `.docx`.

**How it fails.** Loses the distinction between formal and informal registers
(consequential in German, Japanese and many others); mistranslates domain terms
that have an everyday meaning; renders negation and conditionals wrongly in long
sentences; quietly drops or duplicates clauses. Errors are grammatical and
fluent, so proofreading the target language alone will not find them — you have
to compare against the source.

**Do not** use it for anything with legal effect — contracts, filings, consent
forms, safety instructions — without a qualified human translator.

### Chat and question-answering

**What it does.** Answers questions, optionally with your documents as context.

**How it fails.** Fabricates citations, page numbers and quotations that look
right. States things the source documents do not contain. Agrees with an
incorrect premise in your question rather than correcting it. Loses track of
earlier turns in a long conversation.

**How to work with it.** Treat it as a way to *find* things in your corpus, not
as a source. Every claim that matters should be traced back to a document you
open yourself. If it gives you a quotation, search for that quotation.

### Search, deduplication, language detection, tagging

These produce scores and rankings rather than content. They fail by ranking a
poor match highly or missing a good one — visible, recoverable, and much lower
stakes. Semantic search finds conceptually related material, which means it will
sometimes return something relevant that contains none of your search terms, and
sometimes miss an exact phrase. Use keyword search when you need exactness.

### Synthetic speech and images

Speech synthesised on your machine carries a watermark in the audio signal.
Images and speech from a connected AIToolkit backend carry a marking in the
file's metadata, which **re-encoding or format conversion can strip**. The app
tells you which kind you have. If you convert such a file, the marking is your
responsibility again.

---

## Reading a confidence score

Where the app shows one, it is the model's estimate of its own correctness, and
models are systematically overconfident. A high score means "this looks like the
cases I was trained on", not "this is true". Use scores to decide **what to check
first**, never as a substitute for checking.

A low score is more informative than a high one: it reliably indicates a
problem, whereas a high one does not reliably indicate its absence.

---

## What you must not use it for

The intended-purpose statement — shown once in the app, and printable with
`crispsorter intended-purpose show` — excludes specific uses: screening job
applicants, assessing creditworthiness, evaluating students, law-enforcement and
migration purposes, inferring identity or emotions or protected characteristics,
and anything requiring certified accuracy or evidential value.

That list is not a disclaimer. Under **Article 25(1)(c)**, applying this system
to one of those purposes makes *you* the provider of a high-risk AI system, with
the whole of Chapter III attaching to you: conformity assessment, risk
management, logging, human oversight, registration. CrispSorter provides none of
that and cannot be made to by agreement.

The trap is gradual. Nobody sets out to build a CV screener. Somebody sorts a
folder of applications by "relevance" because the tool is right there, and the
purpose has changed without a decision ever being taken. If you are pointing
document classification at people rather than at documents, stop and get advice.

---

## For whoever is responsible for a team

A workable literacy baseline for staff using this app:

1. They can state that outputs are suggestions and that they are the ones
   deciding — not the software.
2. They can name the specific failure mode of the capability they use daily
   (from the sections above).
3. They know which uses are excluded, and why that is about liability rather
   than etiquette.
4. They know how to check: sample before bulk-sorting, compare OCR against the
   image, verify a chat citation by opening the document.
5. They know that AI-generated output leaving the organisation should be
   labelled as such, and that the app marks some artifacts automatically but
   cannot mark all of them (see [`ai-act.md`](ai-act.md) § 6).

None of this needs a training course. It needs the people doing the work to have
read something like this page once, and to have been told that catching a wrong
suggestion is the job rather than an interruption to it.

---

*Not legal advice. This describes the software's behaviour and our reading of
the obligations; your own deployment context is yours to assess.*
