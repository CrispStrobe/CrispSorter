<script lang="ts">
  import { onMount, tick, untrack } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { message as nativeMessage } from "@tauri-apps/plugin-dialog";
  import {
    ScanText,
    FileImage,
    Plus,
    FolderOpen,
    Save,
    Download,
    Play,
    Pause,
    Square,
    RotateCcw,
    RotateCw,
    Undo2,
    Redo2,
    ZoomIn,
    ZoomOut,
    Maximize2,
    Columns2,
    Check,
    CheckCheck,
    ChevronLeft,
    ChevronRight,
    ChevronDown,
    ArrowUp,
    ArrowDown,
    Search,
    X,
    SlidersHorizontal,
    FileText,
    Layers,
    Table2,
    Sigma,
    Braces,
    WandSparkles,
    Crop,
    Scissors,
    MousePointer2,
    Eye,
    EyeOff,
    RefreshCw,
    AlertTriangle,
    CircleCheck,
    Clock3,
    Trash2,
    Copy,
    Keyboard,
    ListFilter,
    LoaderCircle,
    Image,
    PanelLeftClose,
    PanelLeftOpen,
    ScanLine,
    ShieldCheck,
    Camera,
    BookOpen,
  } from "lucide-svelte";
  import PageCanvas from "./ocr/PageCanvas.svelte";
  import PhotoPreparation from "./ocr/PhotoPreparation.svelte";
  import ModelStorage from "./ocr/ModelStorage.svelte";
  import { CORNER_NAMES, captureFileError, initialPhotoQuad, photoQuadError, type PhotoQuad } from "$lib/ocr/photo";
  import { beginBatch, updateBatchPhase, updateBatchPage, batchPageIsCurrent, resumeBatch, type BatchOptions } from "$lib/ocr/batch";
  import WordReview from "./ocr/WordReview.svelte";
  import PageTextEditor from "./ocr/PageTextEditor.svelte";
  import DocumentText from "./ocr/DocumentText.svelte";
  import StructurePanel from "./ocr/StructurePanel.svelte";
  import PreviousStructure from "./ocr/PreviousStructure.svelte";
  import PageTextDraftDialog from "./ocr/PageTextDraftDialog.svelte";
  import ExtractionObjectsPanel from "./ocr/ExtractionObjectsPanel.svelte";
  import TableRepairDialog from "./ocr/TableRepairDialog.svelte";
  import TableJoinDialog from "./ocr/TableJoinDialog.svelte";
  import OfficeExportPanel from "./ocr/OfficeExportPanel.svelte";
  import WorkbookExportPanel from "./ocr/WorkbookExportPanel.svelte";
  import PassageToolsDialog from "./ocr/PassageToolsDialog.svelte";
  import PassageSources from "./ocr/PassageSources.svelte";
  import ProofreadingPanel from "./ocr/ProofreadingPanel.svelte";
  import WordSuggestions from "./ocr/WordSuggestions.svelte";
  import CorrectionPreviewPanel from "./ocr/CorrectionPreview.svelte";
  import {
    emptyProofreadingData, readProofreadingData, createVocabularyEntry,
    mergeProofreadingData, exportProofreadingData, normalizeProofreadingLanguage,
    updateProjectProofreading, suggestCorrections, ignoreSuggestion, isSuggestionIgnored,
    captureCorrectionPreview, applyCorrectionPreview,
    type ProofreadingData, type ProofreadingSelection, type CorrectionSuggestion,
    type CorrectionRule, type CorrectionRequest, type CorrectionPreview,
  } from "$lib/ocr/proofreading";
  import { createProofreadingWriter, type ProofreadingSaveStatus } from "$lib/ocr/proofreadingStore";
  import { hasAlignedCharacterConfidence } from "$lib/ocr/confidence";
  import type { WordEvidence } from "$lib/ocr/words";
  import { joinRegionText, sourceOffsetForTextarea } from "$lib/ocr/text";
  import {
    createPageTextDraft, updatePageTextDraft, discardPageTextDraft,
    preparePageTextEdit, applyPageTextEdit, type PageTextEditPreview,
  } from "$lib/ocr/pageTextDraft";
  import {
    passageEligibility, isPassageBoundary, applyPassagePreview, applyPassageRepair, readSourceComponents,
    type PassagePreview, type RepairPreview,
  } from "$lib/ocr/passages";
  import {
    workspaceClient,
    modelDownloadLabel,
    importProgressLabel,
    exportProgressLabel,
    type WorkspaceClient,
    type WorkspaceCapabilities,
    type ProjectSummary,
    type TransformOptions,
    type ExportRequest,
    type ExportProgress,
    type PhotoAssessment,
  } from "$lib/ocr/client";
  import { ProjectAutosave, type SaveStatus } from "$lib/ocr/autosave";
  import { missingModels, setDefaultOcrLanguage } from "$lib/ocr/client";
  import { i18n } from "$lib/i18n.svelte";
  import { checkExportText, resolveExportFormat } from "$lib/ocr/export";
  import { SequentialQueue, type QueueState } from "$lib/ocr/queue";
  import {
    captureZoneReview,
    prepareZoneReview,
    applyZoneReview,
    type ZoneReviewPreview,
  } from "$lib/ocr/zoneReview";
  import type { PixelRectangle } from "$lib/ocr/zones";
  import type { ExtractionSource } from "$lib/ocr/extraction";
  import { applyTableOperation, type TableOperationPreview, type TableExtractionObject } from "$lib/ocr/tableOperations";
  import { applyContinuationJoin, type ContinuationJoinPreview } from "$lib/ocr/continuationJoin";
  import {
    captureExtractionRequest, appendExtractionObject, getExtractionObjects,
    extractionObjectFreshness, type ExtractionObject,
  } from "$lib/ocr/extractionObjects";
  import { prepareExtractionExport } from "$lib/ocr/extractionExport";
  import { emptyOfficeExportOptions, type OfficeExportOptions } from "$lib/ocr/officeExport";
  import { emptyWorkbookOptions, type WorkbookOptions } from "$lib/ocr/workbookExport";
  import {
    PROFILES,
    createProject,
    createHistory,
    commitHistory,
    undoHistory,
    redoHistory,
    parseProject,
    addDocument,
    updatePage,
    applyRecognition,
    editRegion,
    updateRegion,
    addRegion,
    deleteRegion,
    findSourceDocument,
    moveRegion,
    movePage,
    removePage,
    approvePage,
    applyPageImage,
    splitPage,
    getReviewIssues,
    getMetrics,
    findMatches,
    replaceMatches,
    nextMatch,
    prepareExport,
    type OcrProject,
    type OcrPage,
    type OcrRegion,
    type RegionKind,
    type RecognitionResult,
    type TextMatch,
  } from "$lib/ocr/model";

  let {
    client = workspaceClient,
    initialProject = null,
    active = true,
    initialLocationUri = "",
  }: {
    client?: WorkspaceClient;
    initialProject?: OcrProject | null;
    active?: boolean;
    /** A document sent from another tab ("Open in OCR"). It is added to the
     *  current project once, or selected when that project already has it. */
    initialLocationUri?: string;
  } = $props();
  let history = $state(
    createHistory(untrack(() => initialProject) ?? createProject()),
  );
  const project = $derived(history.present);
  let selectedPageId = $state<string | null>(
    untrack(() => initialProject)?.pages[0]?.id ?? null,
  );
  let selectedRegionId = $state<string | null>(null);
  let wordInspection = $state<{
    projectId: string;
    pageId: string;
    imagePath: string;
    pageWidth: number;
    pageHeight: number;
    regionId: string;
    currentText: string;
    originalText: string;
    regionGeometry: string;
    source: "current" | "original";
    word: WordEvidence;
  } | null>(null);
  let checkedPages = $state<string[]>([]);
  let pageFilter = $state("all");
  let pageQuery = $state("");
  let pageRange = $state("");
  let pageWindowStart = $state(0);
  const pageWindowSize = 100;
  let pageRail = $state(true);
  let mobileView = $state<"pages" | "image" | "tools">("image");
  let photoOpen = $state(false);
  let perspective = $state(false);
  let photoToolsButton = $state<HTMLButtonElement>();
  let photoFocusBack = $state<HTMLButtonElement>();
  let photoFocusCoordinatesOpen = $state(false);
  let photoQuad = $state<PhotoQuad>(initialPhotoQuad(100, 100));
  let photoAssessment = $state<PhotoAssessment | null>(null);
  let photoAngle = $state(0);
  let modelProgress = $state("");
  let modelStorageRevision = $state(0);
  let modelSetupPanel = $state<HTMLElement>();
  let cameraInput = $state<HTMLInputElement>();
  let lastPhotoSource = "";
  let inspector = $state<"review" | "recognition" | "extract" | "export">(
    "review",
  );
  let drawMode = $state<
    "select" | "text" | "table" | "picture" | "formula" | "crop" | "recognize"
  >("select");
  let fit = $state<"page" | "width" | "custom">("page");
  let zoom = $state(1);
  let overlays = $state(true);
  let showOriginal = $state(false);
  let onlyIssues = $state(false);
  type TextView = "document" | "regions" | "text" | "structure";
  const TEXT_VIEW_KEY = "crispsorter.ocr.reviewView";
  /** The page text as one document is the default; the last choice is kept. */
  let textView = $state<TextView>((() => {
    try {
      const saved = globalThis.localStorage?.getItem(TEXT_VIEW_KEY);
      if (saved === "document" || saved === "regions" || saved === "text" || saved === "structure") return saved;
    } catch { /* storage unavailable */ }
    return "document";
  })());
  $effect(() => {
    const view = textView;
    try { globalThis.localStorage?.setItem(TEXT_VIEW_KEY, view); } catch { /* storage unavailable */ }
  });
  let searchOpen = $state(false);
  let query = $state(""),
    replacement = $state(""),
    matchCase = $state(false);
  let currentMatch = $state<TextMatch | null>(null);
  let passageAction = $state<{
    project: OcrProject;
    pageId: string;
    regionId: string;
    mode: "split" | "merge" | "repair";
    offset?: number;
  } | null>(null);
  let tableRepair = $state<{ projectId: string; pageId: string; object: TableExtractionObject; cellId: string | null } | null>(null);
  let tableJoin = $state<{ projectId: string; pageId: string; object: TableExtractionObject } | null>(null);
  let pageTextDraftOpen = $state(false);
  let pageTextDraftLocalUnsaved = $state(false);
  let proofreadingOpen = $state(false);
  let userProofreading = $state<ProofreadingData>(emptyProofreadingData());
  let userProofreadingLoad = $state<"loading" | "ready" | "error" | "unavailable">("loading");
  let userProofreadingError = $state("");
  let userProofreadingStatus = $state<ProofreadingSaveStatus>({ state: "saved", error: null });
  let correctionSelectedIds = $state<string[]>([]);
  let correctionDraft = $state<{ from: string; to: string; language: string; nonce: number } | null>(null);
  let correctionDraftNonce = 0;
  let userProofreadingLoadAttempt = 0;
  let booting = $state(true),
    closing = $state(false),
    busy = $state(""),
    error = $state(""),
    notice = $state("");
  let saveStatus = $state<SaveStatus>({ state: "saved" });
  let importController = $state<AbortController | null>(null);
  let importStopping = $state(false);
  let exportController = $state<AbortController | null>(null);
  let exportStopping = $state(false);
  let exportPhase = $state<ExportProgress["phase"] | null>(null);
  const exportCancellable = $derived(!!exportController && (exportPhase === "rendering" || exportPhase === "writing"));
  let capabilities = $state<WorkspaceCapabilities>({
    ocr: false,
    crispembed: false,
    pdf_import: false,
    formats: ["text"],
    engines: [],
    layout: false,
    kie: false,
    table: false,
    math: false,
  });
  let recent = $state<ProjectSummary[]>([]),
    recentOpen = $state(false),
    helpOpen = $state(false);
  let confirm = $state<{
    title: string;
    message: string;
    label: string;
    action: () => void | Promise<void>;
    splitWidth?: number;
    ocrPreview?: ZoneReviewPreview;
    correctionPreview?: CorrectionPreview;
  } | null>(null);
  let splitPosition = $state(0);
  let queueState = $state<QueueState>({
    phase: "idle",
    total: 0,
    completed: 0,
    failed: 0,
    currentId: null,
    remainingIds: [],
  });
  let queuePromise = $state<Promise<void> | null>(null);
  let runningProjectId = "", runningBatchId = "";
  let checkpointFailed = false;
  let autosave: ProjectAutosave<OcrProject> | null = null;
  let mounted = true;
  const userProofreadingWriter = createProofreadingWriter(async (data) => {
    if (!client.saveUserProofreading) throw new Error("All-project vocabulary storage is unavailable in this build.");
    await client.saveUserProofreading(data);
  }, (status) => { if (mounted) userProofreadingStatus = status; });
  let editGroup: string | null = null;
  let lastReviewIssueKey: string | null = null;
  let editorPane = $state<HTMLDivElement>();
  let actionDialog = $state<HTMLDialogElement>();
  let helpDialog = $state<HTMLDialogElement>();
  $effect(() => {
    if (confirm && actionDialog && !actionDialog.open) actionDialog.showModal();
  });
  $effect(() => {
    if (helpOpen && helpDialog && !helpDialog.open) helpDialog.showModal();
  });
  $effect(() => {
    if (!active) {
      confirm = null;
      passageAction = null;
      tableRepair = null;
      tableJoin = null;
      if (!pageTextDraftLocalUnsaved) pageTextDraftOpen = false;
      helpOpen = false;
      recentOpen = false;
    }
  });
  let exportFormat = $state("pdf"),
    exportScope = $state("all"),
    requireReviewed = $state(true);
  let officeOptions = $state<OfficeExportOptions>(emptyOfficeExportOptions());
  let workbookOptions = $state<WorkbookOptions>(emptyWorkbookOptions());
  let exportResultNotes = $state<{ path: string; format: string; warnings: string[] } | null>(null);
  let pdfScale = $state("source"),
    pdfDpi = $state(300);
  const pdfExportSelected = $derived(exportFormat === "pdf" || exportFormat === "pdfa");
  let labels = $state("vendor, invoice number, date, total, tax"),
    fieldThreshold = $state(0.5),
    useLayoutFields = $state(false);
  let extractionKind = $state<"fields" | "table" | "formula">("fields");
  let extractionSelection = $state<Record<string, string>>({});
  let geometryOpen = $state(false);
  let advancedConfig = $state(""),
    configError = $state("");

  const activePage = $derived(
    project.pages.find((p) => p.id === selectedPageId) ?? project.pages[0],
  );
  $effect(() => {
    const key = `${project.id}:${activePage?.id}:${activePage?.imagePath}`;
    if (key === lastPhotoSource) return;
    lastPhotoSource = key;
    photoAssessment = null;
    photoAngle = 0;
    perspective = false;
    photoQuad = initialPhotoQuad(activePage?.width ?? 100, activePage?.height ?? 100);
  });
  const savedBatchPending = $derived(project.batch?.jobs.filter(job => job.state === "pending" || job.state === "running").length ?? 0);
  const savedBatchFailed = $derived(project.batch?.jobs.filter(job => job.state === "failed").length ?? 0);
  const pageIndex = $derived(
    activePage ? project.pages.findIndex((p) => p.id === activePage.id) : -1,
  );
  const region = $derived(
    activePage?.regions.find((r) => r.id === selectedRegionId),
  );
  const selectedPassageEligibility = $derived(activePage && region ? passageEligibility(activePage, region.id) : null);
  const earlierSourceComponents = $derived(activePage ? readSourceComponents(activePage.provenance.passageSources) ?? [] : []);
  const inspectedWord = $derived.by(() => {
    const selection = wordInspection;
    if (
      !selection || !activePage || !region || showOriginal ||
      drawMode !== "select" || inspector !== "review" || textView !== "regions" ||
      selection.projectId !== project.id || selection.pageId !== activePage.id ||
      selection.imagePath !== activePage.imagePath ||
      selection.pageWidth !== activePage.width || selection.pageHeight !== activePage.height ||
      selection.regionId !== region.id || selection.currentText !== region.text ||
      selection.originalText !== region.originalText ||
      selection.regionGeometry !== `${region.x},${region.y},${region.w},${region.h}`
    ) return null;
    const text = selection.source === "original" ? region.originalText : region.text;
    return text.slice(selection.word.start, selection.word.end) === selection.word.text
      ? selection.word : null;
  });
  const wordSource = $derived(inspectedWord?.bounds ? {
    text: inspectedWord.text,
    start: inspectedWord.sourceStart ?? inspectedWord.start,
    end: inspectedWord.sourceEnd ?? inspectedWord.end,
    bounds: inspectedWord.bounds,
  } : null);
  const loupeViewBox = $derived.by(() => {
    const box = wordSource?.bounds ?? region;
    if (!box || !activePage) return "";
    const x = Math.max(0, box.x - 10), y = Math.max(0, box.y - 6);
    const right = Math.min(activePage.width, box.x + box.w + 10);
    const bottom = Math.min(activePage.height, box.y + box.h + 6);
    return `${x} ${y} ${right - x} ${bottom - y}`;
  });
  $effect(() => {
    if (wordInspection && !inspectedWord) wordInspection = null;
  });
  const projectProofreading = $derived(project.proofreading ?? emptyProofreadingData());
  const proofreadingReady = $derived(userProofreadingLoad === "ready" || userProofreadingLoad === "unavailable");
  const userProofreadingWritable = $derived(userProofreadingLoad === "ready" && !!client.saveUserProofreading);
  const proofreadingSelection = $derived<ProofreadingSelection | null>(
    inspectedWord && wordInspection?.source === "current" && activePage && region &&
    !["picture", "formula", "table"].includes(region.kind) && !region.excluded && !activePage.excluded
      ? { pageId: activePage.id, regionId: region.id, start: inspectedWord.start, end: inspectedWord.end, text: inspectedWord.text }
      : null,
  );
  const proofreadingSuggestions = $derived.by((): { items: CorrectionSuggestion[]; error: string } => {
    if (!proofreadingReady || !proofreadingSelection) return { items: [], error: "" };
    try {
      return { items: suggestCorrections(project, proofreadingSelection, userProofreading, projectProofreading.language), error: "" };
    } catch (failure) {
      return { items: [], error: message(failure) };
    }
  });
  const proofreadingIgnored = $derived(proofreadingSelection ? isSuggestionIgnored(project, proofreadingSelection) : false);
  const metrics = $derived(getMetrics(project));
  const reviewIssues = $derived(getReviewIssues(project));
  const pageIssues = $derived(
    reviewIssues.filter((i) => i.pageId === activePage?.id),
  );
  const issueRegionIds = $derived(
    new Set(pageIssues.flatMap((i) => (i.regionId ? [i.regionId] : []))),
  );
  const visibleRegions = $derived(
    activePage?.regions.filter(
      (r) => !onlyIssues || issueRegionIds.has(r.id),
    ) ?? [],
  );
  const visiblePages = $derived(
    project.pages.filter(
      (p) =>
        (!pageQuery ||
          p.name.toLowerCase().includes(pageQuery.toLowerCase())) &&
        (pageFilter === "all" ||
          (pageFilter === "review" &&
            reviewIssues.some((i) => i.pageId === p.id)) ||
          (pageFilter === "approved" && p.approved) ||
          (pageFilter === "pending" && p.status === "pending") ||
          (pageFilter === "failed" && p.status === "failed") ||
          (pageFilter === "excluded" && p.excluded)),
    ),
  );
  const pageNumbers = $derived(
    new Map(project.pages.map((p, i) => [p.id, i + 1])),
  );
  const issueCounts = $derived.by(() => {
    const counts = new Map<string, number>();
    for (const issue of reviewIssues)
      counts.set(issue.pageId, (counts.get(issue.pageId) ?? 0) + 1);
    return counts;
  });
  const windowPages = $derived(
    visiblePages.slice(pageWindowStart, pageWindowStart + pageWindowSize),
  );
  let lastRailSelection = "";
  $effect(() => {
    const key = JSON.stringify([
      project.id,
      activePage?.id,
      pageFilter,
      pageQuery,
    ]);
    if (key !== lastRailSelection) {
      lastRailSelection = key;
      const index = visiblePages.findIndex((p) => p.id === activePage?.id);
      pageWindowStart =
        index < 0 ? 0 : Math.floor(index / pageWindowSize) * pageWindowSize;
    }
    if (pageWindowStart >= visiblePages.length)
      pageWindowStart = Math.max(
        0,
        Math.floor((visiblePages.length - 1) / pageWindowSize) * pageWindowSize,
      );
  });
  const recognitionWarnings = $derived([
    ...new Set(
      [
        activePage?.provenance.importWarnings,
        activePage?.provenance.warnings,
        activePage?.provenance.preparationWarnings,
      ]
        .flatMap((value) => (Array.isArray(value) ? value : []))
        .filter((value): value is string => typeof value === "string")
        .filter((value) => !ENGINE_NOTE.test(value)),
    ),
  ]);
  /** "Automatic recognition used Tesseract (deu). …" is information, not a problem. */
  const ENGINE_NOTE = /^Automatic recognition used ([^.]+)\./;
  const recognitionEngine = $derived.by(() => {
    const notes = [activePage?.provenance.warnings].flatMap((value) => (Array.isArray(value) ? value : []));
    for (const note of notes) {
      const match = typeof note === "string" ? ENGINE_NOTE.exec(note) : null;
      if (match) return match[1];
    }
    const engine = (activePage?.provenance as Record<string, unknown> | undefined)?.engine;
    return typeof engine === "string" && engine && engine !== "auto" ? engine.replaceAll("_", " ") : null;
  });
  const matches = $derived(
    findMatches(project, query, { caseSensitive: matchCase }),
  );
  const queueActive = $derived(
    ["running", "pausing", "paused", "cancelling"].includes(queueState.phase),
  );
  const locked = $derived(booting || closing || !!busy || queueActive || queuePromise !== null);
  const visibleSaveStatus = $derived<SaveStatus>(saveStatus.state === "error" ? saveStatus
    : userProofreadingStatus.state === "error" ? { state: "error", error: userProofreadingStatus.error ?? "All-project vocabulary save failed." }
    : saveStatus.state === "saving" || userProofreadingStatus.state === "saving" ? { state: "saving" }
    : saveStatus.state === "pending" || userProofreadingStatus.state === "pending" ? { state: "pending" }
    : saveStatus);
  const settings = $derived(project.recognitionSettings ?? {});
  const engine = $derived(
    typeof settings.engine === "string" ? settings.engine : (capabilities.default_engine ?? "auto"),
  );
  const language = $derived(
    typeof settings.language === "string" ? settings.language : (capabilities.default_language ?? "auto"),
  );
  const systemLanguages = $derived(capabilities.system_tesseract?.languages ?? []);
  const recognitionAvailable = $derived(capabilities.ocr && (
    engine === "system_tesseract"
      ? (capabilities.system_tesseract?.available ?? capabilities.engines.includes(engine))
      : capabilities.crispembed
  ));
  // Only meaningful for Qwen2-VL, and only where the worker can be spawned.
  const cpuBudget = $derived(
    settings.cpuPixelBudget === true &&
      engine === "qwen2vl" &&
      !!capabilities.cpu_budget,
  );
  const profile = $derived(
    PROFILES.find((p) => p.id === project.profileId) ?? PROFILES[0],
  );
  const config = $derived({
    ...profile.config,
    ...(settings.config && typeof settings.config === "object"
      ? (settings.config as Record<string, unknown>)
      : {}),
  });
  const exportIds = $derived(
    exportScope === "current"
      ? activePage
        ? [activePage.id]
        : []
      : exportScope === "selected"
        ? checkedPages
        : undefined,
  );
  const preflight = $derived(
    prepareExport(project, { pageIds: exportIds, requireReviewed, format: exportFormat,
      ...(exportFormat === "docx" && capabilities.structured_docx ? { officeOptions } : {}),
      ...(exportFormat === "xlsx" && capabilities.structured_xlsx ? { workbookOptions } : {}) }),
  );
  const exportTextCheck = $derived(checkExportText(exportFormat, capabilities, preflight.pages));
  const exportErrors = $derived([...preflight.errors, ...exportTextCheck.errors]);
  const pageText = $derived(
    joinRegionText(activePage?.regions.filter((r) => !r.excluded && r.kind !== "picture") ?? []),
  );
  const allText = $derived(
    project.pages
      .filter((p) => !p.excluded)
      .map((p) =>
        joinRegionText(p.regions.filter((r) => !r.excluded && r.kind !== "picture")),
      )
      .join("\f"),
  );
  const formatNames: Record<string, string> = {
    pdf: "Searchable PDF",
    pdfa: "Archival PDF/A-2b",
    hocr: "hOCR",
    alto: "ALTO XML",
    text: "Plain text",
    json: "Structured JSON",
    html: "HTML document",
    markdown: "Markdown",
    docx: "Word document",
    xlsx: "Excel workbook",
  };
  const engineNames: Record<string, string> = {
    auto: "Automatic pipeline",
    dbnet_trocr: "DBNet + TrOCR",
    parseq: "PARSeq scene text",
    surya: "Surya multilingual",
    tesseract: "Tesseract LSTM",
    system_tesseract: "System Tesseract · word evidence",
    tesseract_fraktur: "Tesseract LSTM · Fraktur",
    ppocrv6: "PP-OCRv6 (CJK lines)",
    easyocr: "EasyOCR",
    olmocr: "olmOCR 2 (7B)",
    lfm2_vl: "LFM2.5 VL",
    got: "GOT OCR (non-commercial)",
    glm: "GLM OCR",
    qwen2vl: "Qwen2 VL",
    qwen3vl: "Qwen3 VL",
    internvl2: "InternVL2",
    deepseek_ocr2: "DeepSeek OCR 2",
    pix2struct: "Pix2Struct",
    granite_vision: "Granite Vision",
    lightonocr: "LightOnOCR",
    unlimited_ocr: "Unlimited OCR",
  };
  const languageNames: Record<string, string> = {
    eng: "English", deu: "German", fra: "French", spa: "Spanish",
    ita: "Italian", por: "Portuguese", nld: "Dutch", pol: "Polish",
    rus: "Russian", ara: "Arabic", chi_sim: "Chinese, simplified",
    chi_tra: "Chinese, traditional", jpn: "Japanese", kor: "Korean",
  };
  const regionsKinds: RegionKind[] = [
    "text",
    "title",
    "table",
    "picture",
    "formula",
    "header",
    "footer",
  ];

  const queue = new SequentialQueue<RecognitionResult>({
    canStart: () => !busy && recognitionAvailable,
    isCurrent: (job) => project.id === runningProjectId && project.batch?.id === runningBatchId && batchPageIsCurrent(project, job.id),
    onState: (state) => {
      queueState = { ...state, remainingIds: [...state.remainingIds] };
    },
    onStart: async (job) => {
      change(updateBatchPage(updatePage(project, job.id, { status: "running", error: null }), job.id, "running"), false);
      await autosave?.flush();
    },
    onResult: async (job, result) => {
      const next = applyRecognition(project, job.id, result, project.batch?.profileId ?? project.profileId);
      change(updateBatchPage(next, job.id, "completed"), false);
      await autosave?.flush();
    },
    onError: async (job, failure) => {
      const next = updatePage(project, job.id, { status: "failed", error: message(failure), approved: false });
      change(updateBatchPage(next, job.id, "failed", message(failure)), false);
      await autosave?.flush();
    },
    onCheckpointError: (failure, job, stage) => {
      checkpointFailed = true;
      let next = project;
      if (stage === "start") next = updateBatchPage(updatePage(next, job.id, { status: "pending" }), job.id, "pending");
      change(updateBatchPhase(next, "stopped"), false);
      const reason = saveStatus.state === "error" ? "its project checkpoint could not be saved" : "a recognition result or checkpoint could not be accepted";
      error = `Recognition stopped because ${reason}. Your current results remain in this workspace. Resolve the reported problem, save the project, then resume the saved batch. ${message(failure)}`;
    },
  });

  function message(value: unknown) {
    return value instanceof Error ? value.message : String(value);
  }
  function fail(value: unknown) {
    error = message(value);
  }
  function change(next: OcrProject, record = true) {
    if (next === project) return;
    editGroup = null;
    history = record
      ? commitHistory(history, next)
      : { ...history, present: next };
    autosave?.schedule(next);
  }
  function safe(action: () => void) {
    try {
      error = "";
      action();
    } catch (e) {
      fail(e);
    }
  }
  function bindSave(revision = 0) {
    const projectId = project.id;
    autosave = new ProjectAutosave(
      (snapshot, expected) => client.save(snapshot, expected),
      (state) => {
        if (project.id === projectId) saveStatus = state;
      },
      revision,
    );
  }
  async function flush() {
    try {
      await autosave?.flush();
      if (!await userProofreadingWriter.flush()) throw new Error(userProofreadingStatus.error ?? "All-project vocabulary could not be saved. Retry saving it before closing.");
      notice = "Project saved";
      error = "";
    } catch (e) {
      fail(e);
    }
  }
  async function loadUserProofreading() {
    const attempt = ++userProofreadingLoadAttempt;
    if (!client.loadUserProofreading) {
      userProofreadingLoad = "unavailable";
      return;
    }
    userProofreadingLoad = "loading";
    userProofreadingError = "";
    try {
      const data = readProofreadingData(await client.loadUserProofreading());
      if (!mounted || attempt !== userProofreadingLoadAttempt) return;
      userProofreading = data;
      userProofreadingLoad = "ready";
    } catch (failure) {
      if (!mounted || attempt !== userProofreadingLoadAttempt) return;
      userProofreadingLoad = "error";
      userProofreadingError = message(failure);
    }
  }
  function setProjectProofreading(data: ProofreadingData, expectedProjectId = project.id) {
    if (locked) return;
    safe(() => {
      if (project.id !== expectedProjectId) throw new Error("The active project changed. Apply the vocabulary change again in the intended project.");
      change(updateProjectProofreading(project, data));
    });
  }
  function setUserProofreading(data: ProofreadingData) {
    if (locked || !userProofreadingWritable) return;
    safe(() => {
      userProofreading = readProofreadingData(data);
      userProofreadingWriter.schedule(userProofreading);
    });
  }
  function setProofreadingLanguage(value: string) {
    if (locked) return;
    safe(() => change(updateProjectProofreading(project, {
      ...projectProofreading, language: normalizeProofreadingLanguage(value),
    })));
  }
  function addSelectedVocabulary(kind: "word" | "protected", scope: "project" | "user") {
    const selection = proofreadingSelection;
    if (!selection || locked || (scope === "user" && !userProofreadingWritable)) return;
    safe(() => {
      const entry = createVocabularyEntry(selection.text, kind === "protected" ? "und" : projectProofreading.language, kind, true);
      const current = scope === "project" ? projectProofreading : userProofreading;
      const data = mergeProofreadingData(current, { ...emptyProofreadingData(), entries: [entry] });
      if (scope === "project") setProjectProofreading(data); else setUserProofreading(data);
      notice = `${kind === "protected" ? "Protected identifier" : "Vocabulary entry"} added to ${scope === "project" ? "this project" : "all projects"}. OCR scores and approval are unchanged.`;
    });
  }
  function ignoreSelectedSuggestion() {
    if (!proofreadingSelection || locked) return;
    safe(() => change(ignoreSuggestion(project, proofreadingSelection!)));
  }
  function previewCorrections(request: CorrectionRequest) {
    if (locked || !proofreadingReady) return;
    safe(() => {
      const preview = captureCorrectionPreview(project, request, userProofreading);
      correctionSelectedIds = preview.matches.map((match) => match.id);
      confirm = {
        title: "Review proposed corrections",
        message: "Choose the occurrences to change. Protected identifiers are retained, and current source text is checked again when you apply.",
        label: "Apply corrections", correctionPreview: preview,
        action: () => safe(() => {
          const next = applyCorrectionPreview(project, preview, correctionSelectedIds, userProofreading);
          const count = correctionSelectedIds.length;
          change(next);
          currentMatch = null;
          wordInspection = null;
          notice = `Applied ${count} correction${count === 1 ? "" : "s"}. Undo restores the previous text.`;
        }),
      };
    });
  }
  function previewSuggestion(suggestion: CorrectionSuggestion) {
    const selection = proofreadingSelection;
    if (!selection) return;
    previewCorrections({
      from: selection.text, to: suggestion.text, language: projectProofreading.language,
      caseSensitive: true, wholeWord: false, scope: "passage", pageId: selection.pageId,
      regionId: selection.regionId, selection: { ...selection }, includeApproved: true,
    });
  }
  function previewRule(rule: CorrectionRule, scope: CorrectionRequest["scope"], includeApproved: boolean) {
    if (!rule.enabled) return;
    previewCorrections({ from: rule.from, to: rule.to, language: rule.language,
      caseSensitive: rule.caseSensitive, wholeWord: rule.wholeWord, scope, includeApproved,
      pageId: activePage?.id, regionId: region?.id, pageIds: [...checkedPages],
    });
  }
  function rememberCorrection(suggestion: CorrectionSuggestion) {
    if (!proofreadingSelection || locked) return;
    correctionDraft = { from: proofreadingSelection.text, to: suggestion.text,
      language: projectProofreading.language, nonce: ++correctionDraftNonce };
    wordInspection = null;
    proofreadingOpen = true;
  }
  function toggleProofreading() {
    correctionDraft = null;
    wordInspection = null;
    proofreadingOpen = !proofreadingOpen;
  }
  async function exportProofreading(data: ProofreadingData, scope: "project" | "user") {
    if (locked) return;
    const sources = project.documents.map((document) => document.sourceUri);
    busy = "Exporting vocabulary";
    error = "";
    try {
      const text = exportProofreadingData(data);
      const path = await client.pickExport(scope === "project" ? "project-vocabulary" : "user-vocabulary", "json");
      if (!path) return;
      const result = await client.writeText(path, text, sources);
      notice = `Saved ${result.saved_path}`;
    } catch (failure) { fail(failure); }
    finally { busy = ""; }
  }
  async function activate(next: OcrProject, revision = 0) {
    // Validate before closing the current writer: an invalid file must never
    // leave the still-visible project with an autosaver that silently ignores edits.
    const parsed = parseProject(next);
    if (pageTextDraftLocalUnsaved) throw new Error("The page draft contains input that has not been stored. Reduce or copy that input before changing projects.");
    await autosave?.close();
    history = createHistory(parsed);
    selectedPageId = history.present.pages[0]?.id ?? null;
    selectedRegionId = null;
    pageTextDraftOpen = false;
    pageTextDraftLocalUnsaved = false;
    tableRepair = null;
    tableJoin = null;
    proofreadingOpen = false;
    correctionDraft = null;
    checkedPages = [];
    extractionSelection = {};
    officeOptions = emptyOfficeExportOptions();
    workbookOptions = emptyWorkbookOptions();
    exportResultNotes = null;
    lastReviewIssueKey = null;
    saveStatus = { state: "saved" };
    bindSave(revision);
    recentOpen = false;
  }
  async function newProject() {
    if (locked) return;
    busy = "Creating project";
    try {
      await activate(createProject());
      notice = "New project ready";
    } catch (e) {
      fail(e);
    } finally {
      busy = "";
    }
  }
  async function loadProject(id: string) {
    if (locked) return;
    busy = "Opening project";
    error = "";
    try {
      // Flush before reading so reopening this project cannot roll back a
      // debounced correction or install an obsolete optimistic-lock revision.
      await autosave?.flush();
      if (id === project.id) {
        recentOpen = false;
        notice = "Current project saved";
        return;
      }
      const saved = await client.load(id);
      if (!saved) throw new Error("The saved project is no longer available.");
      await activate(saved.project, saved.revision);
      notice = saved.warnings?.join(" · ") || "Project restored";
    } catch (e) {
      fail(e);
    } finally {
      busy = "";
    }
  }
  async function openRecent() {
    recentOpen = !recentOpen;
    if (recentOpen) {
      try {
        recent = await client.list();
      } catch (e) {
        fail(e);
      }
    }
  }
  async function importFiles(paths?: string[]) {
    if (locked) return;
    error = "";
    busy = "Choosing documents";
    try {
      // Resolve an earlier save failure before adding more source documents.
      await autosave?.flush();
      const selected = paths ?? (await client.pickDocuments());
      if (!selected.length) return;
      const controller = new AbortController();
      importController = controller;
      importStopping = false;
      busy = "Importing documents";
      let added = 0;
      let importSaveFailed = false;
      const errors: string[] = [];
      for (const path of selected) {
        if (controller.signal.aborted) break;
        try {
          if (findSourceDocument(project, path)) {
            errors.push(
              `${path.split(/[\\/]/).pop()}: already in this project`,
            );
            continue;
          }
          busy = `Importing ${path.split(/[\\/]/).pop()}`;
          const imported = await client.importDocument(path, { signal: controller.signal,
            onProgress: progress => { if (!controller.signal.aborted) busy = importProgressLabel(progress); } });
          if (findSourceDocument(project, imported.source_uri, imported.archive_uri)) {
            errors.push(`${imported.name}: this original document is already in the project`);
            continue;
          }
          const next = addDocument(project, imported);
          if (!project.pages.length) {
            next.name = imported.name.replace(/\.[^.]+$/, "") || "OCR project";
            selectedPageId = next.pages[0].id;
          }
          change(next);
          added += imported.pages.length;
          try {
            await autosave?.flush();
          } catch (e) {
            // The autosaver retains this snapshot for Retry. Stop the import
            // before accepting another document into an unsaved project.
            importSaveFailed = true;
            errors.push(`${imported.name}: project save failed: ${message(e)}`);
            break;
          }
        } catch (e) {
          if (controller.signal.aborted || (e instanceof DOMException && e.name === "AbortError")) break;
          errors.push(`${path.split(/[\\/]/).pop()}: ${message(e)}`);
        }
      }
      notice = `${added} ${added === 1 ? "page" : "pages"} added${importSaveFailed
        ? ". Import stopped because saving failed. Use Save to retry; keep this workspace open."
        : controller.signal.aborted ? ". Import cancelled; completed documents are saved." : ""}`;
      if (errors.length) error = errors.join("\n");
    } catch (e) {
      fail(e);
    } finally {
      importController = null;
      importStopping = false;
      busy = "";
    }
  }
  async function capturePhoto(event: Event) {
    const input = event.currentTarget as HTMLInputElement;
    const file = input.files?.[0];
    input.value = "";
    if (!file || locked || !client.importImage) return;
    error = "";
    const invalid = captureFileError(file, capabilities.image_import_formats);
    if (invalid) { error = invalid; return; }
    busy = "Importing photo";
    const controller = new AbortController();
    importController = controller;
    importStopping = false;
    try {
      const imported = await client.importImage(file.name || "Camera photo.jpg", new Uint8Array(await file.arrayBuffer()), {
        signal: controller.signal, onProgress: progress => { if (!controller.signal.aborted) busy = importProgressLabel(progress); },
      });
      const existing = findSourceDocument(project, imported.source_uri, imported.archive_uri);
      if (existing) {
        const first = project.pages.find(page => page.documentId === existing.id);
        if (first) selectPage(first.id);
        notice = "This original photo is already in the project.";
        return;
      }
      const before = project.pages.length;
      const next = addDocument(project, imported);
      if (!before) next.name = imported.name.replace(/\.[^.]+$/, "") || "Photo project";
      change(next);
      selectPage(next.pages[before].id);
      photoOpen = true;
      mobileView = "image";
      await autosave?.flush();
      notice = "Photo added. Check its edges, tilt and lighting before recognition.";
    } catch (failure) {
      if (controller.signal.aborted || (failure instanceof DOMException && failure.name === "AbortError")) notice = "Photo import cancelled.";
      else fail(failure);
    }
    finally { busy = ""; importController = null; importStopping = false; }
  }
  function cancelImport() {
    if (!importController || importStopping) return;
    importStopping = true;
    busy = "Stopping import after the current step…";
    importController.abort();
  }
  async function assessPhoto() {
    const page = activePage;
    if (!page || locked || !client.assessPhoto) return;
    busy = "Checking photo quality";
    error = "";
    try {
      const result = await client.assessPhoto(page.imagePath);
      if (activePage?.id !== page.id || activePage.imagePath !== page.imagePath) return;
      photoAssessment = result;
      photoAngle = result.correction_degrees ?? 0;
      notice = result.correction_degrees === null ? "Inspect the page and adjust tilt manually if needed." : "Tilt estimated. Apply the correction, then inspect the preview.";
    } catch (failure) { fail(failure); }
    finally { busy = ""; }
  }
  function showPhotoTools() {
    photoOpen = !photoOpen;
    mobileView = "image";
    showOriginal = false;
    drawMode = "select";
    if (!photoOpen) perspective = false;
  }
  function setPerspective(enabled: boolean) {
    const leavingPhoneFocus = !enabled && !!photoFocusBack?.getClientRects().length;
    perspective = enabled;
    showOriginal = false;
    drawMode = "select";
    mobileView = "image";
    if (enabled) {
      fit = "page";
      photoFocusCoordinatesOpen = false;
    }
    void tick().then(() => {
      if (enabled && perspective && photoFocusBack?.getClientRects().length) {
        photoFocusBack.focus({ preventScroll: true });
      } else if (leavingPhoneFocus) {
        photoToolsButton?.focus({ preventScroll: true });
      }
    });
  }
  function setPhotoCoordinate(index: number, axis: "x" | "y", value: number) {
    photoQuad = photoQuad.map((point, i) => i === index ? { ...point, [axis]: value } : { ...point }) as PhotoQuad;
  }
  async function applyPerspective() {
    if (!activePage) return;
    const invalid = photoQuadError(photoQuad, activePage.width, activePage.height);
    if (invalid) { error = invalid; return; }
    await transform("perspective", { quad: photoQuad.map(point => ({ ...point })) as PhotoQuad });
  }
  async function flattenShadows() {
    await prepareImage({
      denoise: false, dewarp: false, restore: false, sr: false, cleanup_enabled: true,
      stages: [{ cleanup: { enabled: true, deskew: false, crop_borders: false, whiten_background: true,
        binarize: false, despeckle: false, blackfilter: false, auto_crop: false, page_split: false } }],
    });
  }
  function openInspector(panel: typeof inspector) {
    inspector = panel;
    mobileView = "tools";
  }
  // Handle each hand-off once, after the last project has been restored (so
  // the document joins it instead of being replaced by it) and while no batch,
  // save or import holds the workspace (`locked` also covers booting).
  let handledLocationUri = "";
  $effect(() => {
    const uri = initialLocationUri;
    if (!uri || locked || uri === handledLocationUri) return;
    handledLocationUri = uri;
    untrack(() => void openRequestedDocument(uri));
  });
  async function openRequestedDocument(uri: string) {
    const existing = findSourceDocument(project, uri);
    const firstPage =
      existing && project.pages.find((p) => p.documentId === existing.id);
    if (firstPage) {
      selectPage(firstPage.id);
      notice = `${existing.name} is already in this project`;
      return;
    }
    const before = project.pages.length;
    await importFiles([uri]);
    const added = project.pages[before];
    if (added) selectPage(added.id);
  }
  function selectPage(id: string) {
    lastReviewIssueKey = null;
    selectedPageId = id;
    selectedRegionId = null;
    wordInspection = null;
    editGroup = null;
    showOriginal = false;
    drawMode = "select";
    currentMatch = null;
  }
  function confirmRemovePage() {
    const page = activePage;
    if (!page || locked) return;
    const originalIndex = pageIndex;
    confirmation(
      "Remove this page?",
      `${page.name} will be removed from this project. The original is preserved and Undo can restore this page.`,
      "Remove page",
      () => {
        change(removePage(project, page.id));
        selectPage(
          project.pages[Math.min(originalIndex, project.pages.length - 1)]
            ?.id ?? "",
        );
      },
    );
  }
  function confirmSplitPage() {
    const page = activePage;
    if (!page || locked) return;
    splitPosition = Math.floor(page.width / 2);
    confirmation(
      "Split this spread?",
      `Split ${page.name} at a vertical line. Each half becomes an independent page. Existing recognition is kept in Undo.${page.structure ? " Current formatting and links must be reviewed after recognizing the new pages. Previous document structure remains available in the saved history." : ""}`,
      "Split page",
      () => transform("split", { splitX: Math.round(splitPosition) }, page.id, true),
      page.width,
    );
  }
  function gotoPage(delta: number) {
    const next = project.pages[pageIndex + delta];
    if (next) selectPage(next.id);
  }
  async function selectRegion(id: string) {
    mobileView = "tools";
    lastReviewIssueKey = null;
    selectedRegionId = id;
    wordInspection = null;
    proofreadingOpen = false;
    inspector = "review";
    textView = "regions";
    if (onlyIssues && !issueRegionIds.has(id)) onlyIssues = false;
    await tick();
    editorPane
      ?.querySelector<HTMLElement>(`[data-region-id="${id}"]`)
      ?.scrollIntoView({ block: "nearest", behavior: "auto" });
  }
  async function selectWord(r: OcrRegion, word: WordEvidence) {
    if (!activePage || locked || r.excluded) return;
    const projectId = project.id;
    const pageId = activePage.id;
    const imagePath = activePage.imagePath;
    const sourceText = r.text;
    if (sourceText.slice(word.start, word.end) !== word.text) return;
    currentMatch = null;
    await selectRegion(r.id);
    // The selection is valid only for the text version that supplied its
    // offsets. Navigation or an edit during the render must not target another
    // page, a changed word, or original OCR offsets in corrected text.
    if (
      project.id !== projectId || activePage?.id !== pageId ||
      activePage.imagePath !== imagePath || selectedRegionId !== r.id ||
      activePage.regions.find((item) => item.id === r.id)?.text !== sourceText
    ) return;
    const editor = editorPane?.querySelector<HTMLTextAreaElement>(
      `[data-region-id="${r.id}"] textarea[data-region-text]`,
    );
    editor?.focus();
    editor?.setSelectionRange(word.start, word.end);
    inspectWord(r, word, "current");
  }
  function inspectWord(r: OcrRegion, word: WordEvidence | null, source: "current" | "original") {
    if (!word) {
      wordInspection = null;
      return;
    }
    const page = activePage;
    const current = page?.regions.find((item) => item.id === r.id);
    if (!page || !current || selectedRegionId !== r.id) return;
    const text = source === "original" ? current.originalText : current.text;
    if (
      current.text !== r.text || current.originalText !== r.originalText ||
      word.start < 0 || word.end <= word.start || word.end > text.length ||
      text.slice(word.start, word.end) !== word.text
    ) return;
    const box = word.bounds;
    const validBounds = box && [box.x, box.y, box.w, box.h].every(Number.isFinite) &&
      box.x >= 0 && box.y >= 0 && box.w > 0 && box.h > 0 &&
      box.x + box.w <= page.width && box.y + box.h <= page.height;
    showOriginal = false;
    drawMode = "select";
    wordInspection = {
      projectId: project.id, pageId: page.id, imagePath: page.imagePath,
      pageWidth: page.width, pageHeight: page.height, regionId: current.id,
      currentText: current.text, originalText: current.originalText,
      regionGeometry: `${current.x},${current.y},${current.w},${current.h}`,
      source, word: { ...word, bounds: validBounds ? { ...box } : undefined },
    };
  }
  async function editPassage(r: OcrRegion) {
    if (!activePage || locked || r.excluded || r.kind === "picture") return;
    const pageId = activePage.id;
    currentMatch = null;
    wordInspection = null;
    proofreadingOpen = false;
    selectedRegionId = r.id;
    inspector = "review";
    textView = "text";
    editGroup = null;
    await tick();
    if (activePage?.id !== pageId || selectedRegionId !== r.id) return;
    const editor = editorPane?.querySelector<HTMLTextAreaElement>(
      `[data-passage-id="${r.id}"] textarea[data-passage-text]`,
    );
    editor?.focus();
    editor?.scrollIntoView({ block: "nearest" });
  }
  function readRegionCaret(id: string) {
    const article = Array.from(editorPane?.querySelectorAll<HTMLElement>("[data-region-id]") ?? [])
      .find((item) => item.dataset.regionId === id);
    const editor = article?.querySelector<HTMLTextAreaElement>("textarea[data-region-text]");
    return editor ? { start: editor.selectionStart, end: editor.selectionEnd, text: editor.value } : null;
  }
  function openPassageAction(
    mode: "split" | "merge" | "repair",
    target: OcrRegion,
    caret: { start: number; end: number; text: string } | null = null,
  ) {
    const page = activePage;
    if (!page || locked || confirm || passageAction || pageTextDraftOpen || tableRepair || tableJoin) return;
    const current = page.regions.find((item) => item.id === target.id);
    if (!current) return;
    try {
      const eligibility = passageEligibility(page, current.id);
      let splitOffset: number | null = null;
      if (mode === "split") {
        if (!eligibility.splitAllowed) throw new Error(eligibility.reasons.join(" "));
        splitOffset = caret ? sourceOffsetForTextarea(current.text, caret.text, caret.start) : null;
        if (!caret || caret.start !== caret.end || splitOffset === null ||
          !isPassageBoundary(current.text, splitOffset) ||
          !current.text.slice(0, splitOffset).trim() || !current.text.slice(splitOffset).trim())
          throw new Error("Place the caret inside this passage, with no text selected and complete text on both sides, then choose Split passage.");
      } else if (mode === "merge" && !eligibility.nextMergeId) {
        throw new Error("Merge needs the immediately following included passage of the same kind, with no intervening region.");
      } else if (mode === "repair" && (page.excluded || current.excluded || !current.text.trim() || !["text", "title", "header", "footer"].includes(current.kind))) {
        throw new Error("Choose an included, nonempty text passage to repair.");
      }
      // Keep the selected source frozen while preview options change. Applying
      // compares this exact page snapshot with the current project again.
      const capturedPage = JSON.parse(JSON.stringify(page)) as OcrPage;
      passageAction = {
        project: { ...project, pages: project.pages.map((item) => item.id === page.id ? capturedPage : item) },
        pageId: page.id, regionId: current.id, mode,
        ...(mode === "split" && splitOffset !== null ? { offset: splitOffset } : {}),
      };
      currentMatch = null;
      wordInspection = null;
      editGroup = null;
      error = "";
    } catch (failure) { fail(failure); }
  }
  function finishPassageChange(pageId: string, regionId: string, text: string) {
    selectedPageId = pageId;
    selectedRegionId = regionId;
    currentMatch = null;
    wordInspection = null;
    drawMode = "select";
    showOriginal = false;
    inspector = "review";
    proofreadingOpen = false;
    passageAction = null;
    error = "";
    notice = text;
  }
  function applyStructuralPassage(preview: PassagePreview): string | null {
    if (locked) return "Wait for the current operation to finish, then apply the passage edit.";
    try {
      change(applyPassagePreview(project, preview));
      finishPassageChange(preview.page.id, preview.regions[0].id, preview.request.kind === "split" ? "Passage split. Original sources are available in Source components." : "Passages merged. Original sources are available in Source components.");
      return null;
    } catch (failure) { return message(failure); }
  }
  function applyTextRepair(preview: RepairPreview, selectedIds: string[]): string | null {
    if (locked) return "Wait for the current operation to finish, then apply the text repair.";
    try {
      change(applyPassageRepair(project, preview, selectedIds));
      finishPassageChange(preview.page.id, preview.regionIds[0], `${selectedIds.length} text repair${selectedIds.length === 1 ? "" : "s"} applied. Undo restores the previous text.`);
      return null;
    } catch (failure) { return message(failure); }
  }
  function openPageTextDraft() {
    if (!activePage || locked || confirm || passageAction || pageTextDraftOpen || tableRepair || tableJoin) return;
    try {
      change(createPageTextDraft(project, activePage.id), false);
      resumePageTextDraft();
    } catch (failure) { fail(failure); }
  }
  function resumePageTextDraft() {
    if (!project.pageTextDraft || locked || confirm || passageAction || tableRepair || tableJoin) return;
    currentMatch = null;
    wordInspection = null;
    editGroup = null;
    error = "";
    pageTextDraftOpen = true;
  }
  function updateOpenPageTextDraft(text: string): string | null {
    if (locked) return "Wait for the current document operation to finish before updating the draft.";
    try { change(updatePageTextDraft(project, text), false); return null; }
    catch (failure) { return message(failure); }
  }
  function discardOpenPageTextDraft(): string | null {
    if (locked) return "Wait for the current document operation to finish before discarding the draft.";
    try {
      change(discardPageTextDraft(project), false);
      pageTextDraftOpen = false;
      pageTextDraftLocalUnsaved = false;
      notice = "Unapplied page text draft discarded.";
      return null;
    } catch (failure) { return message(failure); }
  }
  function applyOpenPageTextDraft(preview: PageTextEditPreview, acceptCombinedCoverage: boolean): string | null {
    if (locked) return "Wait for the current document operation to finish before applying the draft.";
    try {
      change(applyPageTextEdit(project, preview, acceptCombinedCoverage));
      selectedPageId = preview.page.id;
      selectedRegionId = preview.replacementIds[0] ?? null;
      currentMatch = null;
      wordInspection = null;
      editGroup = null;
      textView = "text";
      inspector = "review";
      pageTextDraftOpen = false;
      pageTextDraftLocalUnsaved = false;
      notice = "Page text draft applied. Review the changed passage before approving; Undo restores the previous page and draft.";
      return null;
    } catch (failure) { return message(failure); }
  }
  function drawRecognitionArea() {
    if (!activePage || locked || !recognitionAvailable) return;
    drawMode = "recognize";
    showOriginal = false;
    notice = "Draw an area on the page. You can compare the new OCR text before using it.";
  }
  function togglePage(id: string) {
    checkedPages = checkedPages.includes(id)
      ? checkedPages.filter((p) => p !== id)
      : [...checkedPages, id];
  }
  function applyPageRange() {
    const ids = new Set<string>();
    try {
      for (const part of pageRange
        .split(",")
        .map((s) => s.trim())
        .filter(Boolean)) {
        if (!/^\d+(?:\s*-\s*\d+)?$/.test(part))
          throw new Error(
            "Use page numbers and ranges, for example 1–4, 8. Enter a hyphen for a range.",
          );
        const [a, b] = part.split("-").map(Number);
        const end = b ?? a;
        if (a < 1 || end < a || end > project.pages.length)
          throw new Error(
            `Page ranges must be between 1 and ${project.pages.length}.`,
          );
        for (let i = a; i <= end; i++) ids.add(project.pages[i - 1].id);
      }
      checkedPages = [...ids];
      error = "";
    } catch (e) {
      fail(e);
    }
  }
  function confirmation(
    title: string,
    body: string,
    label: string,
    action: () => void | Promise<void>,
    splitWidth?: number,
  ) {
    confirm = { title, message: body, label, action, splitWidth };
  }
  function undo() {
    if (locked) return;
    history = undoHistory(history);
    autosave?.schedule(project);
    selectedRegionId = null;
    editGroup = null;
  }
  function redo() {
    if (locked) return;
    history = redoHistory(history);
    autosave?.schedule(project);
    selectedRegionId = null;
    editGroup = null;
  }
  function editText(r: OcrRegion, text: string) {
    if (!activePage || locked) return;
    const key = `${activePage.id}:${r.id}`;
    const next = editRegion(project, activePage.id, r.id, text);
    history =
      editGroup === key
        ? { ...history, present: next }
        : commitHistory(history, next);
    editGroup = key;
    autosave?.schedule(next);
  }
  function patchRegion(patch: Partial<OcrRegion>) {
    if (activePage && region)
      safe(() =>
        change(updateRegion(project, activePage.id, region.id, patch)),
      );
  }
  function reviewNext() {
    if (!reviewIssues.length) {
      lastReviewIssueKey = null;
      notice = "No outstanding review issues";
      return;
    }
    const keyFor = (issue: (typeof reviewIssues)[number]) =>
      JSON.stringify([issue.pageId, issue.regionId, issue.reason, issue.message]);
    const cursor = reviewIssues.findIndex((issue) => keyFor(issue) === lastReviewIssueKey &&
      issue.pageId === activePage?.id && issue.regionId === selectedRegionId);
    const current = cursor >= 0 ? cursor : reviewIssues.findIndex(
      (issue) => issue.pageId === activePage?.id && issue.regionId === selectedRegionId,
    );
    const issue = reviewIssues[(current + 1) % reviewIssues.length];
    selectPage(issue.pageId);
    if (issue.regionId) void selectRegion(issue.regionId);
    else {
      const extractionIssue = issue.reason === "changed" && issue.message === "Extracted content needs page approval.";
      openInspector(extractionIssue ? "extract" : "review");
      const page = project.pages.find((item) => item.id === issue.pageId);
      if (extractionIssue && page) {
        const candidates = getExtractionObjects(page).collection.objects.filter((object) => !object.excluded && extractionObjectFreshness(project, object).current);
        const selected = candidates.find((object) => object.id === extractionSelection[page.id + ":" + extractionKind]) ?? candidates[0];
        if (selected) {
          extractionKind = selected.kind;
          extractionSelection = { ...extractionSelection, [page.id + ":" + selected.kind]: selected.id };
        }
      } else if (issue.reason === "changed" && page?.structure) textView = "structure";
    }
    lastReviewIssueKey = keyFor(issue);
  }
  function approveRegion() {
    if (activePage && region) {
      safe(() =>
        change(
          updateRegion(project, activePage.id, region.id, {
            review: "approved",
          }),
        ),
      );
      reviewNext();
    }
  }
  function approveCurrent() {
    if (activePage)
      safe(() => {
        change(approvePage(project, activePage.id, !activePage.approved));
        notice = activePage.approved
          ? "Page approved"
          : "Page approval removed";
      });
  }
  function setSetting(key: string, value: unknown) {
    change({
      ...project,
      recognitionSettings: { ...settings, [key]: value },
      updatedAt: new Date().toISOString(),
    });
  }
  async function openMobileModels() {
    openInspector("recognition");
    await tick();
    modelSetupPanel?.scrollIntoView({ block: "nearest" });
    modelSetupPanel?.querySelector<HTMLButtonElement>("button")?.focus();
  }
  async function installBasicModels() {
    if (locked || !capabilities.mobile_model_store || !client.installMobileModels) return;
    busy = "Installing OCR models";
    error = ""; notice = ""; modelProgress = "Checking model installation…";
    try {
      const names = await client.installMobileModels(language === "auto" ? "eng" : language, (progress) => {
        if (mounted) modelProgress = modelDownloadLabel(progress);
      });
      if (mounted) {
        notice = `Basic OCR models are installed (${names.join(", ")}). ${engine === "tesseract" ? "Ready to recognize this document." : "Select Tesseract LSTM to use them."}`;
        modelStorageRevision += 1;
      }
    } catch (value) {
      if (mounted) fail(value);
    } finally {
      if (mounted) { busy = ""; modelProgress = ""; }
    }
  }
  function setConfig(key: string, value: unknown) {
    setSetting("config", {
      ...((settings.config as object) ?? {}),
      [key]: value,
    });
  }
  function chooseProfile(id: string) {
    change({
      ...project,
      profileId: id,
      recognitionSettings: { ...settings, config: {} },
      updatedAt: new Date().toISOString(),
    });
    advancedConfig = "";
  }
  function applyAdvanced() {
    try {
      const value = JSON.parse(advancedConfig);
      if (!value || typeof value !== "object" || Array.isArray(value))
        throw new Error("Enter a configuration object.");
      setSetting("config", value);
      configError = "";
      notice = "Advanced configuration applied";
    } catch (e) {
      configError = message(e);
    }
  }
  async function recognize(
    scope: "current" | "selected" | "pending" | "all" | "failed",
    confirmed = false,
    confirmedIds?: string[],
  ) {
    if (locked || !recognitionAvailable) return;
    const pages = project.pages.filter(
      (p) =>
        !p.excluded &&
        (confirmedIds
          ? confirmedIds.includes(p.id)
          : scope === "current"
            ? p.id === activePage?.id
            : scope === "selected"
              ? checkedPages.includes(p.id)
              : scope === "pending"
                ? p.status === "pending" || p.status === "failed"
                : scope === "failed"
                  ? p.status === "failed"
                  : true),
    );
    if (!pages.length) {
      notice = "No pages match this recognition scope";
      return;
    }
    if (
      !confirmed &&
      pages.some(
        (p) =>
          p.approved || p.structure ||
          p.regions.some(
            (r) => r.text !== r.originalText || r.review === "approved",
          ),
      )
    ) {
      confirmation(
        "Replace reviewed recognition?",
        `${pages.length} pages will be recognized with the current profile. Existing corrections can be recovered with Undo.${pages.some((page) => page.structure) ? " New recognition replaces the current block formatting and links. Previous document structure remains inspectable in saved history." : ""}`,
        "Recognize again",
        () =>
          recognize(
            scope,
            true,
            pages.map((p) => p.id),
          ),
      );
      return;
    }
    error = "";
    selectedRegionId = null;
    showOriginal = false;
    const options: BatchOptions = { profileId: project.profileId, config: JSON.parse(JSON.stringify(config)), engine, language, cpuPixelBudget: cpuBudget };
    change(beginBatch(project, pages.map(page => page.id), options));
    await runPreparedBatch(pages.map(page => page.id), options);
  }
  async function finishQueueSegment() {
    const phase = queueState.phase === "paused" ? "paused" : queueState.phase === "cancelled" ? "stopped" : "completed";
    let next = project;
    for (const id of queueState.remainingIds) {
      if (next.pages.find(page => page.id === id)?.status === "running") {
        next = updateBatchPage(updatePage(next, id, { status: "pending" }), id, "pending");
      }
    }
    change(updateBatchPhase(next, phase), false);
    if (checkpointFailed) return;
    await autosave?.flush();
    notice = phase === "paused" ? "Queue paused. Completed pages are saved." : phase === "stopped"
      ? "Queue stopped. Resume the saved batch to finish its original selection."
      : `${queueState.completed} pages recognized${queueState.failed ? `, ${queueState.failed} failed. Use Retry failed.` : ""}`;
  }
  async function runPreparedBatch(pageIds: string[], options: BatchOptions) {
    busy = "Saving recognition batch";
    checkpointFailed = false;
    try {
      // Persist exact intent and settings before the first native invocation.
      await autosave?.flush();
      runningProjectId = project.id;
      runningBatchId = project.batch?.id ?? "";
      busy = "";
      const started = queue.start(pageIds.map(id => ({
        id,
        run: async () => {
          if (!batchPageIsCurrent(project, id)) throw new Error("The saved page changed. Start a new recognition selection.");
          const current = project.pages.find(page => page.id === id)!;
          return client.recognize({ pagePath: current.imagePath, pageWidthPoints: current.widthPoints,
            pageHeightPoints: current.heightPoints, config: options.config, engine: options.engine, language: options.language, cpuPixelBudget: options.cpuPixelBudget ?? false });
        },
      })));
      if (!started) throw new Error("The recognition queue is already active.");
      queuePromise = queue.whenSettled();
      await queuePromise;
      await finishQueueSegment();
    } catch (failure) {
      change(updateBatchPhase(project, "stopped"), false);
      fail(failure);
    } finally { busy = ""; queuePromise = null; }
  }
  async function resumeSavedBatch(retryFailed = false) {
    if (locked || !capabilities.ocr) return;
    error = "";
    const plan = resumeBatch(project, { retryFailed });
    if (plan.conflicts.length) { error = plan.conflicts.join("\n"); return; }
    if (!plan.pageIds.length || !plan.options) { notice = "No unfinished pages in the saved batch"; return; }
    change(plan.project);
    await runPreparedBatch(plan.pageIds, plan.options);
  }
  async function stopQueue() {
    const paused = queue.state.phase === "paused";
    // The previous segment owns its final checkpoint until it clears this lock.
    if (paused && queuePromise) return;
    queue.cancel();
    if (!paused) return; // A running segment saves its stopped phase on settlement.
    const stopped = Promise.resolve().then(() => finishQueueSegment());
    queuePromise = stopped;
    try { await stopped; }
    catch (failure) { fail(failure); }
    finally { if (queuePromise === stopped) queuePromise = null; }
  }
  async function resumeQueue() {
    if (busy || queuePromise) return;
    busy = "Saving recognition checkpoint";
    checkpointFailed = false;
    try {
      change(updateBatchPhase(project, "running"), false);
      await autosave?.flush();
      busy = "";
      if (!queue.resume()) return;
      queuePromise = queue.whenSettled();
      await queuePromise;
      await finishQueueSegment();
    } catch (failure) { change(updateBatchPhase(project, "paused"), false); fail(failure); }
    finally { busy = ""; queuePromise = null; }
  }
  function changeExtraction(next: OcrProject, groupKey?: string) {
    if (locked || next === project) return;
    const record = !groupKey || editGroup !== groupKey;
    change(next, record);
    editGroup = groupKey ?? null;
  }
  function selectExtraction(id: string) {
    if (!activePage) return;
    editGroup = null;
    extractionSelection = { ...extractionSelection, [activePage.id + ":" + extractionKind]: id };
  }
  function openTableRepair(object: ExtractionObject, cellId: string | null) {
    if (!activePage || locked || confirm || helpOpen || passageAction || pageTextDraftOpen || tableRepair || tableJoin) return;
    const current = currentExtraction(object);
    if (!current || current.kind !== "table") {
      error = "Select a saved table on the current page.";
      return;
    }
    currentMatch = null;
    wordInspection = null;
    editGroup = null;
    error = "";
    tableRepair = {
      projectId: project.id, pageId: activePage.id, object: current,
      cellId: current.currentData.cells.some((cell) => cell.id === cellId) ? cellId : null,
    };
  }
  function applyTableRepair(preview: TableOperationPreview): string | null {
    if (locked) return "Wait for the current document operation to finish before applying table changes.";
    if (!tableRepair || tableRepair.projectId !== project.id || tableRepair.pageId !== preview.pageId || tableRepair.object.id !== preview.objectId) {
      return "The source table changed. Your repair choices have been kept; review them again before applying.";
    }
    try {
      change(applyTableOperation(project, preview));
      selectedPageId = preview.pageId;
      extractionKind = "table";
      extractionSelection = { ...extractionSelection, [preview.pageId + ":table"]: preview.objectId };
      inspector = "extract";
      notice = "Table structure updated. Review the changed table before page approval; Undo restores the previous table in one step.";
      return null;
    } catch (failure) { return message(failure); }
  }
  function openTableJoin(object: ExtractionObject) {
    if (!activePage || locked || confirm || helpOpen || passageAction || pageTextDraftOpen || tableRepair || tableJoin) return;
    const current = currentExtraction(object);
    if (!current || current.kind !== "table" || !current.currentData.rows || !current.currentData.columns) {
      error = "Select a nonempty saved table to begin a continuation join.";
      return;
    }
    currentMatch = null;
    wordInspection = null;
    editGroup = null;
    error = "";
    tableJoin = { projectId: project.id, pageId: activePage.id, object: current };
  }
  function applyTableJoin(preview: ContinuationJoinPreview): string | null {
    if (locked) return "Wait for the current document operation to finish before creating the joined table.";
    if (!tableJoin || tableJoin.projectId !== project.id || preview.projectId !== project.id) {
      return "The source project changed. Your join choices have been kept; review them again before creating the table.";
    }
    try {
      const destinationPageId = preview.request.inputs[0].pageId;
      change(applyContinuationJoin(project, preview));
      selectedPageId = destinationPageId;
      selectedRegionId = null;
      wordInspection = null;
      currentMatch = null;
      lastReviewIssueKey = null;
      extractionKind = "table";
      extractionSelection = { ...extractionSelection, [destinationPageId + ":table"]: preview.result.id };
      inspector = "extract";
      notice = "Joined table created. Source tables remain separate. Review the result before page approval; Undo restores the project before this join.";
      return null;
    } catch (failure) { return message(failure); }
  }
  let lastExtractionPageKey = "";
  $effect(() => {
    const page = activePage;
    const key = project.id + ":" + (page?.id ?? "");
    if (key === lastExtractionPageKey) return;
    lastExtractionPageKey = key;
    const fields = page ? getExtractionObjects(page).collection.objects.slice().reverse().find((object) => object.kind === "fields") : undefined;
    if (fields?.kind === "fields" && fields.currentData.labels.length) labels = fields.currentData.labels.join(", ");
  });
  async function transform(
    operation: TransformOptions["operation"],
    options: Partial<TransformOptions> = {},
    pageId = activePage?.id,
    confirmed = false,
  ) {
    if (!pageId || locked) return;
    const page = project.pages.find((p) => p.id === pageId);
    if (!page) return;
    if (!confirmed && page.structure?.blocks.length) {
      confirmation(
        "Change the source page image?",
        `${page.name} has document structure. This image operation clears current recognition, formatting, and links until the new image is recognized and reviewed. Previous document structure remains inspectable in saved history; Undo restores this edit.`,
        "Apply image operation",
        () => transform(operation, options, page.id, true),
      );
      return;
    }
    busy = "Preparing page";
    error = "";
    try {
      const result = await client.transform({
        pagePath: page.imagePath,
        pageWidthPoints: page.widthPoints,
        pageHeightPoints: page.heightPoints,
        operation,
        ...options,
      });
      const first = result.pages[0];
      const pixelsChanged = result.pages.length !== 1 || !first || first.image_path !== page.imagePath || first.width !== page.width || first.height !== page.height;
      if (result.pages.length > 1) {
        const before = project.pages.findIndex((p) => p.id === page.id);
        change(splitPage(project, page.id, result.pages));
        if (selectedPageId === page.id) selectPage(project.pages[before].id);
      } else if (result.pages[0]) {
        change(applyPageImage(project, page.id, result.pages[0], operation));
        if (selectedPageId === page.id) selectedRegionId = null;
      } else throw new Error("The operation produced no pages.");
      showOriginal = false;
      drawMode = "select";
      notice = pixelsChanged
        ? "Page updated. Recognize the new pixels before export."
        : "The page pixels are unchanged. Reviewed text and approval were retained.";
    } catch (e) {
      fail(e);
    } finally {
      busy = "";
    }
  }
  $effect(() => { setDefaultOcrLanguage(i18n.lang === "de" ? "deu" : "eng"); });
  /** A finished batch summary can be dismissed until the next batch. */
  let queueBarDismissedFor = $state<unknown>(null);
  /** Undo of the last preparation stays offered until another edit. */
  let undoPreparationDepth = $state<number | null>(null);
  const canUndoPreparation = $derived(undoPreparationDepth !== null && history.past.length === undoPreparationDepth);
  function undoPreparation() {
    if (!canUndoPreparation || locked) return;
    undo();
    undoPreparationDepth = null;
    notice = "Preparation undone. The previous page image, text and approval were restored.";
  }
  async function prepareImage(override?: Record<string, unknown>, confirmed = false, pageId = activePage?.id, allowDownloads = false) {
    if (!pageId || locked || !capabilities.crispembed) return;
    const page = project.pages.find((candidate) => candidate.id === pageId);
    if (!page) return;
    if (!confirmed && page.structure?.blocks.length) {
      confirmation(
        "Prepare this structured page?",
        `${page.name} has document structure. If preparation changes the image, its current recognition, formatting, and links need to be rebuilt and reviewed. Previous document structure remains inspectable in saved history; Undo restores the prior page.`,
        "Prepare page",
        () => prepareImage(override, true, page.id, allowDownloads),
      );
      return;
    }
    busy = "Preparing the page…";
    error = "";
    const started = Date.now();
    let stage = "Starting";
    const tick = setInterval(() => { busy = `Preparing: ${stage} · ${Math.round((Date.now() - started) / 1000)}s`; }, 1000);
    try {
      const result = await client.prepare(
        page.imagePath,
        override ?? config,
        page.widthPoints,
        page.heightPoints,
        { allowDownloads, onStage: (next) => { stage = next; busy = `Preparing: ${stage} · ${Math.round((Date.now() - started) / 1000)}s`; } },
      );
      const warnings = (result.warnings ?? []).filter(
        (value): value is string => typeof value === "string",
      );
      const pixelsChanged =
        result.image_path !== page.imagePath ||
        result.width !== page.width ||
        result.height !== page.height;
      // Managed paths are content-addressed. A failed or unnecessary repair can
      // return the original pixels; retain their corrected text and approval.
      const next = pixelsChanged
        ? applyPageImage(project, page.id, result, "scan repair")
        : project;
      const updatedPage = next.pages.find((item) => item.id === page.id);
      if (!updatedPage) return;
      change(
        updatePage(next, page.id, {
          provenance: {
            ...updatedPage.provenance,
            preparationWarnings: warnings,
            lastPreparation: {
              sourceImagePath: page.imagePath,
              resultImagePath: result.image_path,
              pixelsChanged,
              warnings,
              completedAt: new Date().toISOString(),
            },
          },
        }),
      );
      if (pixelsChanged) {
        if (selectedPageId === page.id) selectedRegionId = null;
        showOriginal = false;
        undoPreparationDepth = history.past.length;
      }
      notice = [
        pixelsChanged
          ? "Scan repair applied. Use Original to compare, or Undo to restore."
          : "Scan repair made no pixel changes. Reviewed text and approval were retained.",
        ...warnings,
      ].join(" · ");
    } catch (e) {
      const missing = missingModels(e);
      if (missing && !allowDownloads) {
        const list = missing.map((m) => `${m.name} (${m.purpose}${m.license ? `, licence ${m.license}` : ""})`).join(", ");
        busy = "";
        confirmation(
          missing.length === 1 ? "Download the model for this preparation?" : "Download the models for this preparation?",
          `This preparation needs ${list}, which ${missing.length === 1 ? "is" : "are"} not on this computer yet. Download now and continue? Downloads are verified before use.`,
          "Download and prepare",
          () => prepareImage(override, true, page.id, true),
        );
        return;
      }
      fail(e);
    } finally {
      clearInterval(tick);
      if (busy.startsWith("Preparing")) busy = "";
    }
  }
  async function recognizeZone(rect: PixelRectangle, pageId = activePage?.id) {
    if (!pageId || locked || !recognitionAvailable) return;
    error = "";
    try {
      const request = captureZoneReview(project, pageId, rect);
      const configSnapshot = JSON.parse(JSON.stringify(config));
      const engineSnapshot = engine;
      const languageSnapshot = language;
      const cpuBudgetSnapshot = cpuBudget;
      const profileSnapshot = project.profileId;
      const page = request.page;
      busy = "Recognizing selected area";
      showOriginal = false;
      const result = await client.recognize({
        pagePath: page.imagePath,
        pageWidthPoints: page.widthPoints,
        pageHeightPoints: page.heightPoints,
        config: configSnapshot,
        engine: engineSnapshot,
        language: languageSnapshot,
        zone: request.zone,
        cpuPixelBudget: cpuBudgetSnapshot,
      });
      const preview = prepareZoneReview(project, request, result, profileSnapshot);
      if (!mounted || !active) {
        notice = "Partial OCR finished without replacing text. Open the OCR workspace and run it again to review the result.";
        return;
      }
      const zone = request.zone;
      confirm = {
        title: "Review new OCR text",
        message: `${page.name}. The area covers ${zone.w} × ${zone.h} pixels at (${zone.x}, ${zone.y}) and ${request.affectedRegionIds.length} existing text regions. Intersecting passages are covered in full. Compare the result before replacing their text.`,
        label: "Use new OCR text",
        ocrPreview: preview,
        action: () => {
          safe(() => {
            change(applyZoneReview(project, preview));
            selectPage(page.id);
            inspector = "review";
            textView = "regions";
            const first = preview.regions[0];
            if (first) void selectRegion(first.id);
            notice = "New OCR text applied to the selected area. Review it before approval; Undo restores the previous text.";
          });
        },
      };
      notice = "Partial OCR is ready to compare. Current text has been kept.";
    } catch (e) {
      fail(e);
    } finally {
      busy = "";
      drawMode = "select";
    }
  }
  async function onRectangle(
    rect: { x: number; y: number; w: number; h: number },
    mode: string,
  ) {
    if (!activePage || locked) return;
    if (mode === "crop") {
      await transform("crop", { crop: rect });
      return;
    }
    if (mode === "recognize") {
      await recognizeZone(rect, activePage.id);
      return;
    }
    safe(() => {
      change(addRegion(project, activePage.id, rect, mode as RegionKind));
      selectedRegionId =
        activePage.regions[activePage.regions.length - 1]?.id ?? null;
      inspector = "review";
      drawMode = "select";
    });
  }
  async function findNext(direction: -1 | 1 = 1) {
    const match = nextMatch(matches, currentMatch, direction);
    if (!match) return;
    selectedPageId = match.pageId;
    selectedRegionId = match.regionId;
    currentMatch = match;
    onlyIssues = false;
    inspector = "review";
    textView = "regions";
    proofreadingOpen = false;
    await tick();
    const field = editorPane?.querySelector<HTMLTextAreaElement>(
      `[data-region-id="${match.regionId}"] textarea[data-region-text]`,
    );
    field?.focus();
    field?.setSelectionRange(match.start, match.end);
    field?.scrollIntoView({ block: "nearest" });
  }
  function replaceOne() {
    if (!currentMatch || !activePage) return;
    // Stored selection offsets are not authority after the query or text changes.
    const match = matches.find(
      (m) =>
        m.pageId === currentMatch!.pageId &&
        m.regionId === currentMatch!.regionId &&
        m.start === currentMatch!.start &&
        m.end === currentMatch!.end &&
        m.text === currentMatch!.text,
    );
    if (!match || match.pageId !== activePage.id) {
      currentMatch = null;
      void findNext();
      return;
    }
    const r = activePage.regions.find((r) => r.id === match.regionId);
    if (!r) return;
    safe(() =>
      change(
        editRegion(
          project,
          activePage.id,
          r.id,
          r.text.slice(0, match.start) + replacement + r.text.slice(match.end),
        ),
      ),
    );
    currentMatch = null;
    void findNext();
  }
  async function exportProject(
    overwrite = false,
    path?: string,
    snapshot?: Omit<ExportRequest, "outputPath" | "overwrite">,
  ) {
    if (locked) return;
    error = "";
    exportResultNotes = null;
    let exportMode = { format: exportFormat, pdfa: false };
    if (!snapshot) {
      try { exportMode = resolveExportFormat(exportFormat, capabilities); }
      catch (cause) { error = cause instanceof Error ? cause.message : String(cause); return; }
    }
    const textCheck = snapshot
      ? checkExportText(snapshot.pdfa ? "pdfa" : snapshot.format, capabilities, snapshot.pages)
      : exportTextCheck;
    const errors = [...(!snapshot ? preflight.errors : []), ...textCheck.errors];
    if (errors.length) {
      error = errors.join("\n");
      return;
    }
    if (
      !snapshot &&
      pdfExportSelected &&
      pdfScale === "dpi" &&
      (!Number.isFinite(pdfDpi) || pdfDpi < 36 || pdfDpi > 2400)
    ) {
      error = "Enter a scan resolution between 36 and 2400 DPI.";
      return;
    }
    const request =
      snapshot ??
      (JSON.parse(
        JSON.stringify({
          ...exportMode,
          pages:
            pdfExportSelected && pdfScale === "dpi"
              ? preflight.pages.map((p) => ({
                  ...p,
                  page_width_points: (p.width * 72) / pdfDpi,
                  page_height_points: (p.height * 72) / pdfDpi,
                }))
              : preflight.pages,
          sourceUris: project.documents.map((d) => d.sourceUri),
          ...(exportFormat === "docx" && capabilities.structured_docx ? { officeOptions: preflight.officeOptions ?? emptyOfficeExportOptions() } : {}),
          ...(exportFormat === "xlsx" && capabilities.structured_xlsx ? { workbookOptions: preflight.workbookOptions ?? emptyWorkbookOptions() } : {}),
        }),
      ) as Omit<ExportRequest, "outputPath" | "overwrite">);
    const ext =
      ({ text: "txt", markdown: "md", alto: "xml" } as Record<string, string>)[
        request.format
      ] ?? request.format;
    let target = path;
    const controller = new AbortController();
    exportController = controller;
    exportStopping = false;
    exportPhase = null;
    busy = "Saving project before export";
    try {
      // Finish saving before allocating a mobile staging destination.
      await autosave?.flush();
      if (controller.signal.aborted) throw new DOMException("Document export cancelled.", "AbortError");
      busy = "Choosing export destination";
      target =
        path ??
        (await client.pickExport(
          project.name.replace(/[^\p{L}\p{N}._ -]/gu, "_"),
          ext,
        )) ??
        undefined;
      if (!target) return;
      exportPhase = "rendering";
      busy = exportProgressLabel({ request_id: "", phase: "rendering", bytes: 0 });
      const result = await client.exportDocument({
        ...request,
        outputPath: target,
        overwrite,
      }, {
        signal: controller.signal,
        onProgress: progress => {
          if (exportController !== controller) return;
          exportPhase = progress.phase;
          if (progress.phase === "committing" || progress.phase === "destination") exportStopping = false;
          busy = exportStopping ? "Stopping export after the current step…" : exportProgressLabel(progress);
        },
      });
      if (result.cancelled) notice = "Export cancelled";
      else {
        const warnings = [...new Set((result.warnings ?? []).filter((item): item is string => typeof item === "string" && !!item))];
        exportResultNotes = { path: result.saved_path, format: result.format, warnings };
        const sheetCount = result.sheets ?? request.workbookOptions?.sheets.length ?? 0;
        const exportedCount = result.format === "xlsx"
          ? `${sheetCount} ${sheetCount === 1 ? "worksheet" : "worksheets"}`
          : `${result.pages} pages`;
        notice = `Exported ${exportedCount} · ${result.saved_path}${warnings.length ? ` · ${warnings.length} export notes` : ""}`;
      }
    } catch (e) {
      if (e instanceof DOMException && e.name === "AbortError") {
        notice = "Export cancelled";
      } else if (
        !overwrite &&
        target &&
        /already exists|file exists/i.test(message(e))
      ) {
        const chosen = target;
        confirmation(
          "Replace the existing export?",
          `The file ${chosen} already exists. Replace it with the ${request.format === "xlsx" ? `${request.workbookOptions?.sheets.length ?? 0} worksheets` : `${request.pages.length} pages`} selected for this export?`,
          "Replace export",
          () => exportProject(true, chosen, request),
        );
      } else fail(e);
    } finally {
      exportController = null;
      exportStopping = false;
      exportPhase = null;
      busy = "";
    }
  }
  function cancelExport() {
    if (!exportController || !exportCancellable || exportStopping) return;
    exportStopping = true;
    busy = "Stopping export after the current step…";
    exportController.abort();
  }
  async function extractionPath(source: ExtractionSource) {
    if (source.bounds) {
      const result = await client.transform({
        pagePath: source.imagePath,
        operation: "crop",
        crop: source.bounds,
      });
      if (!result.pages[0])
        throw new Error("The selected region could not be cropped.");
      return result.pages[0].image_path;
    }
    return source.imagePath;
  }
  async function extract(kind: "fields" | "table" | "formula") {
    if (!activePage || locked) return;
    const pageId = activePage.id;
    const regionId = region?.id ?? null;
    const layout = useLayoutFields;
    const threshold = fieldThreshold;
    const labelSnapshot = labels;
    busy = "Extracting " + kind;
    error = "";
    extractionKind = kind;
    try {
      const request = captureExtractionRequest(project, pageId, regionId, kind === "fields" && !layout ? "text" : "pixels");
      const source = request.capture;
      let output: unknown;
      let labelList: string[] = [];
      if (kind === "fields") {
        labelList = labelSnapshot.split(",").map((value) => value.trim()).filter(Boolean);
        if (!labelList.length) throw new Error("Enter at least one field label.");
        if (!Number.isFinite(threshold) || threshold < 0 || threshold > 1) throw new Error("Field score must be between zero and one.");
        const result = layout
          ? await client.layoutFields(await extractionPath(source), labelList, threshold)
          : await client.extractFields(source.text ?? "", labelList, threshold);
        output = result;
        notice = result.length + " fields extracted" + (layout ? " from page layout" : " from reviewed text");
      } else if (kind === "table") {
        const result = await client.table(await extractionPath(source));
        output = result;
        notice = result.rows + " rows × " + result.cols + " columns extracted";
      } else {
        const result = await client.math(await extractionPath(source));
        if (result === null || result === "") {
          notice = "No formula was recognized. Existing saved formulas are unchanged.";
          return;
        }
        output = result;
        notice = "Formula recognized";
      }
      // The request captures its project/page/region before recognition. A
      // delayed result stays with that source, even after selection changes.
      const next = appendExtractionObject(project, request, kind, output, kind === "fields" ? { labels: labelList } : undefined);
      const page = next.pages.find((item) => item.id === pageId);
      if (!page) throw new Error("The source page no longer exists.");
      const object = getExtractionObjects(page).collection.objects.find((item) => item.id === request.objectId);
      const freshness = object ? extractionObjectFreshness(next, object) : null;
      change(next);
      if (activePage?.id === pageId) {
        extractionSelection = { ...extractionSelection, [pageId + ":" + kind]: request.objectId };
      } else notice += " · saved to " + page.name;
      if (freshness && !freshness.current) notice += " · Historical source: " + freshness.reasons.join(" ");
    } catch (failure) {
      fail(failure);
    } finally {
      busy = "";
    }
  }
  async function copy(text: string) {
    try {
      await navigator.clipboard.writeText(text);
      notice = "Copied to clipboard";
    } catch {
      error =
        "Clipboard access was unavailable. Select and copy the text manually.";
    }
  }
  function currentExtraction(object: ExtractionObject): ExtractionObject | undefined {
    return activePage ? getExtractionObjects(activePage).collection.objects.find((item) => item.id === object.id) : undefined;
  }
  async function copyExtraction(object: ExtractionObject) {
    if (locked) return;
    try {
      const current = currentExtraction(object);
      if (!current) throw new Error("Select a saved extraction on the current page.");
      await copy(prepareExtractionExport(current).text);
    } catch (failure) { fail(failure); }
  }
  async function saveExtraction(object: ExtractionObject) {
    if (locked) return;
    try {
      const current = currentExtraction(object);
      if (!current) throw new Error("Select a saved extraction on the current page.");
      const sources = project.documents.map((document) => document.sourceUri);
      const { extension, text } = prepareExtractionExport(current);
      busy = "Saving extracted data";
      const outputPath = await client.pickExport(project.name + "-" + current.name, extension);
      if (!outputPath) return;
      const result = await client.writeText(outputPath, text, sources);
      notice = result.cancelled ? "Export cancelled" : `Saved ${result.saved_path}`;
    } catch (e) {
      fail(e);
    } finally {
      busy = "";
    }
  }
  async function reindex() {
    if (!activePage || locked) return;
    const source = project.documents.find(
      (d) => d.id === activePage.documentId,
    );
    if (!source) return;
    const sourcePages = project.pages.filter((p) => p.documentId === source.id);
    const check = prepareExport(
      { ...project, pages: sourcePages },
      { requireReviewed: true },
    );
    if (check.errors.length) {
      error = check.errors.join("\n");
      return;
    }
    busy = "Updating search index";
    error = "";
    try {
      await client.reindex(source.sourceUri, check.text);
      notice = "Search index updated with the approved text";
    } catch (e) {
      fail(e);
    } finally {
      busy = "";
    }
  }
  function keydown(event: KeyboardEvent) {
    if (!active || confirm || helpOpen || passageAction || pageTextDraftOpen || tableRepair || tableJoin) return;
    const target = event.target as HTMLElement;
    const editing = target?.matches(
      'input,textarea,select,[contenteditable="true"]',
    );
    // Structured text and extraction edits restore their metadata and text
    // together from project history. Full-page drafts keep native textarea undo.
    const projectEditor = target?.matches('[data-extraction-editor]') ||
      (activePage?.structure && target?.matches('textarea[data-region-text],textarea[data-passage-text],textarea[data-structure-text]'));
    if (!event.isComposing && projectEditor &&
      (event.metaKey || event.ctrlKey) && !event.altKey && ['z', 'y'].includes(event.key.toLowerCase())) {
      event.preventDefault();
      const selectedId = selectedRegionId;
      event.key.toLowerCase() === "y" || event.shiftKey ? redo() : undo();
      if (selectedId && activePage?.regions.some((item) => item.id === selectedId)) selectedRegionId = selectedId;
      return;
    }
    if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "s") {
      event.preventDefault();
      void flush();
      return;
    }
    if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "f") {
      event.preventDefault();
      proofreadingOpen = false;
      searchOpen = true;
      openInspector("review");
      return;
    }
    if (
      !editing &&
      (event.metaKey || event.ctrlKey) &&
      event.key.toLowerCase() === "z"
    ) {
      event.preventDefault();
      event.shiftKey ? redo() : undo();
      return;
    }
    if (
      !editing &&
      (event.metaKey || event.ctrlKey) &&
      event.key.toLowerCase() === "y"
    ) {
      event.preventDefault();
      redo();
      return;
    }
    if (event.key === "F8") {
      event.preventDefault();
      reviewNext();
      return;
    }
    if (event.key === "Escape") {
      drawMode = "select";
      if (perspective) setPerspective(false);
      confirm = null;
      helpOpen = false;
      recentOpen = false;
      return;
    }
    if (!editing && event.altKey && event.key === "ArrowRight") {
      event.preventDefault();
      gotoPage(1);
    }
    if (!editing && event.altKey && event.key === "ArrowLeft") {
      event.preventDefault();
      gotoPage(-1);
    }
    if (!editing && event.key === "?") {
      helpOpen = true;
    }
  }
  function beforeUnload(event: BeforeUnloadEvent) {
    if (queueActive || queuePromise || busy || pageTextDraftLocalUnsaved || autosave?.hasUnsavedChanges() || userProofreadingWriter.hasUnsavedChanges()) {
      event.preventDefault();
      event.returnValue = "";
    }
  }
  onMount(() => {
    mounted = true;
    let unlisten: (() => void) | undefined;
    let unlistenClose: (() => void) | undefined;
    bindSave();
    void loadUserProofreading();
    void (async () => {
      const results = await Promise.allSettled([
        client.capabilities(),
        initialProject ? Promise.resolve(null) : client.load(),
      ]);
      if (!mounted) return;
      if (results[0].status === "fulfilled") {
        capabilities = results[0].value;
        if (!capabilities.formats.includes(exportFormat))
          exportFormat = capabilities.formats[0] ?? "text";
      } else fail(results[0].reason);
      if (results[1].status === "fulfilled" && results[1].value) {
        const saved = results[1].value;
        try {
          await activate(saved.project, saved.revision);
          notice = saved.warnings?.join(" · ") || "Last project restored";
        } catch (e) {
          fail(e);
        }
      } else if (results[1].status === "rejected") fail(results[1].reason);
      booting = false;
    })();
    // Native drag/drop supplies real filesystem paths; web File names are not paths.
    if (client === workspaceClient)
      void listen<{ paths: string[] }>("tauri://drag-drop", (event) => {
        if (active && !locked && !passageAction && !pageTextDraftOpen && !tableRepair && !tableJoin) void importFiles(event.payload.paths);
      })
        .then((fn) => {
          if (mounted) unlisten = fn;
          else fn();
        })
        .catch(() => {});
    if (client === workspaceClient)
      void getCurrentWindow()
        .onCloseRequested(async (event) => {
          if (closing) {
            event.preventDefault();
            return;
          }
          if (pageTextDraftLocalUnsaved) {
            event.preventDefault();
            await nativeMessage(
              "The open page draft contains input that could not be stored. Reduce the unsupported input or copy it before explicitly discarding the draft, then close the window again.",
              { title: "Page draft has unsaved input", kind: "error" },
            );
            return;
          }
          if (busy || queuePromise || (queueActive && queueState.phase !== "paused")) {
            event.preventDefault();
            await nativeMessage(
              "A document operation is still running. Stop the recognition queue or wait for the current operation to finish, then close the window again. Completed pages are saved automatically.",
              { title: "OCR work is still running", kind: "info" },
            );
            return;
          }
          closing = true;
          if (queueState.phase === "paused") {
            queue.cancel();
            change(updateBatchPhase(project, "stopped"), false);
          }
          try {
            // Tauri waits for this handler before honoring the user's close.
            await autosave?.flush();
            if (!await userProofreadingWriter.flush()) throw new Error(userProofreadingStatus.error ?? "All-project vocabulary could not be saved.");
          } catch (e) {
            event.preventDefault();
            closing = false;
            fail(e);
            await nativeMessage(
              `The project or vocabulary could not be saved. The window remains open so changes can be recovered.\n\n${message(e)}`,
              { title: "Workspace save failed", kind: "error" },
            );
          }
        })
        .then((fn) => {
          if (mounted) unlistenClose = fn;
          else fn();
        })
        .catch(fail);
    return () => {
      mounted = false;
      importController?.abort();
      exportController?.abort();
      unlisten?.();
      unlistenClose?.();
      queue.cancel();
      void queue
        .whenSettled()
        .then(() => Promise.allSettled([autosave?.close(), userProofreadingWriter.close()]))
        .catch(() => {});
    };
  });
</script>

{#snippet mobileModelSetup()}
{#if capabilities.mobile_model_store && client.installMobileModels}
  <section bind:this={modelSetupPanel} class="model-install" aria-label="On-device OCR models">
    <h3>On-device OCR models</h3>
    <p class="field-hint">Install the text detector and selected Tesseract language before recognition. English is used for Model default. Downloads are verified and stored on this device.</p>
    <button class="button" onclick={installBasicModels} disabled={locked}>
      <Download size={16} /> Install basic OCR models
    </button>
    {#if modelProgress}<p class="model-progress" role="status" aria-live="polite">{modelProgress}</p>{/if}
    <p class="field-hint">Other engines and learned image tools need their own models. Install them from the CrispEmbed registry in Settings. Missing models never start an automatic download on mobile.</p>
    <ModelStorage {client} disabled={locked} revision={modelStorageRevision} onerror={fail} onnotice={(value) => { notice = value; error = ""; }} />
  </section>
{/if}
{/snippet}

<svelte:window onkeydown={keydown} onbeforeunload={beforeUnload} />

<section class="ocr-workspace" class:photo-focusing={photoOpen && perspective && mobileView === "image"} aria-label="OCR document workspace">
  <!-- Android's native chooser requires the image/* token to enter camera capture.
       Unsupported image formats are checked after selection, before import. -->
  <input class="camera-input" bind:this={cameraInput} type="file" accept="image/*" capture="environment" aria-label="Camera photo file" onchange={capturePhoto} />
  <div class="workspace-top">
  <header class="workspace-header">
    <div class="workspace-mark"><ScanText size={22} /></div>
    <div class="workspace-title">
      <div class="eyebrow">
        DOCUMENT WORKSPACE <span class="local-badge">LOCAL</span>
      </div>
      <input
        aria-label="Project name"
        value={project.name}
        disabled={locked}
        onchange={(e) =>
          change({
            ...project,
            name: e.currentTarget.value.trim() || "Untitled OCR project",
            updatedAt: new Date().toISOString(),
          })}
      />
    </div>
    <div
      class="save-state"
      data-save-state={visibleSaveStatus.state}
      class:save-error={visibleSaveStatus.state === "error"}
      title={visibleSaveStatus.error ??
        "Project, corrections, and vocabulary are saved on this device"}
    >
      {#if visibleSaveStatus.state === "saving"}<LoaderCircle
          size={13}
          class="spin"
        />{:else if visibleSaveStatus.state === "error"}<AlertTriangle
          size={13}
        />{:else if visibleSaveStatus.state === "pending"}<Clock3
          size={13}
        />{:else}<ShieldCheck size={13} />{/if}
      <span id={"ocr-save-status-" + project.id} class:visually-hidden={visibleSaveStatus.state === "saved"}
        >{visibleSaveStatus.state === "saved"
          ? "All changes saved"
          : visibleSaveStatus.state === "pending"
            ? "Unsaved changes"
            : visibleSaveStatus.state === "saving"
              ? "Saving…"
              : "Save failed"}</span
      >
      {#if visibleSaveStatus.state === "error"}<button onclick={flush}>Retry</button
        >{/if}
    </div>
    <div class="header-actions">
      <button
        class="icon-button shortcuts-button"
        onclick={() => (helpOpen = true)}
        title="Keyboard shortcuts"
        aria-label="Keyboard shortcuts"><Keyboard size={17} /></button
      ><button class="icon-button" onclick={openRecent} disabled={locked} aria-label="Projects" title="Saved projects"
        ><FolderOpen size={16} /></button
      ><button class="icon-button" onclick={newProject} disabled={locked} aria-label="New" title="New project"
        ><Plus size={16} /></button
      ><button class="icon-button" onclick={flush} disabled={booting} aria-label="Save" title="Save project"
        aria-describedby={"ocr-save-status-" + project.id}
        ><Save size={16} /></button
      >
    </div>
  </header>

  <div class="command-bar">
    <button
      class="button primary"
      onclick={() => importFiles()}
      disabled={locked}><Plus size={16} /> Add documents</button
    >
    {#if capabilities.camera_import && client.importImage}<button class="icon-button" onclick={() => cameraInput?.click()} disabled={locked} aria-label="Capture photo" title="Capture photo"><Camera size={16} /></button>{/if}
    {#if capabilities.mobile_model_store && client.installMobileModels}<button class="button" onclick={openMobileModels} disabled={locked}><Download size={16} /> OCR models</button>{/if}
    <div class="command-separator"></div>
    <button
      class="button"
      onclick={() => recognize("current")}
      disabled={locked ||
        !activePage ||
        !recognitionAvailable ||
        activePage.excluded}
      title="Recognize the current page using the selected profile"
      ><ScanLine size={16} /> Recognize page</button
    >
    <button
      class="button"
      onclick={() => recognize(checkedPages.length ? "selected" : "pending")}
      disabled={locked || !project.pages.length || !recognitionAvailable}
      ><Play size={14} />
      {checkedPages.length
        ? `Process ${checkedPages.length} selected`
        : "Process pending"}</button
    >
    <button
      class="button subtle"
      onclick={() => openInspector(inspector === "recognition" ? "review" : "recognition")}
      class:active={inspector === "recognition"}
      title={`Recognition settings · ${profile.name}`}
      aria-label="Recognition settings"
      ><SlidersHorizontal size={15} /></button
    >
    <div class="toolbar-spacer"></div>
    <div class="undo-group">
      <button
        class="icon-button"
        onclick={undo}
        disabled={locked || !history.past.length}
        title="Undo (Ctrl/Cmd Z)"
        aria-label="Undo"><Undo2 size={17} /></button
      ><button
        class="icon-button"
        onclick={redo}
        disabled={locked || !history.future.length}
        title="Redo (Ctrl/Cmd Shift Z)"
        aria-label="Redo"><Redo2 size={17} /></button
      >
    </div>
    <button
      class="button export-button"
      onclick={() => openInspector("export")}
      disabled={!project.pages.length}
      ><Download size={15} /> Export <ChevronRight size={14} /></button
    >
  </div>
  </div>

  {#if project.pageTextDraft}<div class="page-text-draft-banner" data-page-text-draft-banner>
      <FileText size={17} /><div><strong>Unapplied page text draft</strong><span>{project.pageTextDraft.page.name} · {saveStatus.state === "saved" ? "Draft saved" : saveStatus.state === "saving" ? "Saving draft…" : saveStatus.state === "pending" ? "Draft waiting to save" : "Draft save failed"}</span></div>
      <button class="button small" type="button" onclick={resumePageTextDraft} disabled={locked}>Resume page text draft</button>
    </div>{/if}

  {#if recentOpen}
    <div class="project-menu" role="region" aria-label="Saved projects">
      <div class="section-heading">
        <strong>Saved projects</strong><button
          class="icon-button"
          onclick={() => (recentOpen = false)}
          aria-label="Close projects"><X size={15} /></button
        >
      </div>
      {#if recent.length === 0}<p class="muted">
          Your projects appear here after their first save.
        </p>{/if}
      {#each recent as item}<button
          class="project-option"
          onclick={() => loadProject(item.id)}
          ><FileText size={17} /><span
            ><strong>{item.name}</strong><small
              >{item.page_count} pages · {new Date(
                item.updated_at,
              ).toLocaleString()}</small
            ></span
          >{#if item.id === project.id}<Check size={15} />{/if}</button
        >{/each}
    </div>
  {/if}
  {#if error}<div class="feedback error" role="alert">
      <AlertTriangle size={17} /><span>{error}</span><button
        class="icon-button"
        onclick={() => (error = "")}
        aria-label="Dismiss error"><X size={15} /></button
      >
    </div>{/if}
  {#if saveStatus.state === "error"}<div class="feedback error" role="alert">
      <AlertTriangle size={16} /><span
        >Project could not be saved: {saveStatus.error}. Keep this workspace
        open and retry.</span
      >
    </div>{/if}

  {#if !queueActive && project.batch && (savedBatchPending || savedBatchFailed)}
    <div class="batch-recovery" role="region" aria-label="Saved recognition batch">
      <div><strong>{project.batch.phase === "interrupted" ? "Interrupted batch recovered" : "Saved recognition batch"}</strong><span>{savedBatchPending} unfinished · {savedBatchFailed} failed · original selection and settings retained</span></div>
      {#if savedBatchPending}<button class="button small" onclick={() => resumeSavedBatch()} disabled={locked || !capabilities.ocr}><Play size={13} /> Resume saved batch</button>{/if}
      {#if savedBatchFailed}<button class="button small" onclick={() => resumeSavedBatch(true)} disabled={locked || !capabilities.ocr}><RefreshCw size={13} /> Retry saved failures</button>{/if}
    </div>
  {/if}
  {#if booting}
    <div class="loading-state">
      <LoaderCircle size={30} class="spin" />
      <h2>Opening your workspace</h2>
      <p>Checking recognition tools and saved projects…</p>
    </div>
  {:else if !project.pages.length}
    <div class="welcome">
      <div class="welcome-main">
        <div class="welcome-icon"><ScanText size={40} strokeWidth={1.4} /></div>
        <div class="eyebrow">FROM SCAN TO SEARCHABLE DOCUMENT</div>
        <h1>Every page. Every detail.</h1>
        <p>
          Recognize, refine, and review your documents in one place.<br />Keep
          the page image and the text side by side, from first scan to final
          export.
        </p>
        <button
          class="button primary large"
          onclick={() => importFiles()}
          disabled={locked}><Plus size={18} /> Add documents</button
        >
        <div class="supported">{#if capabilities.pdf_import}PDF · {/if}TIFF · PNG · JPEG · BMP · WebP{#each capabilities.image_import_formats ?? [] as format} · {format.toUpperCase()}{/each}</div>
        {@render mobileModelSetup()}
        {#if !capabilities.ocr}<div class="capability-note">
            <AlertTriangle size={16} /><span
              >{capabilities.reason ??
                "This build does not include the CrispEmbed recognition engine. You can open saved projects, review text, and use the available export formats."}</span
            >
          </div>{/if}
      </div>
      <div class="welcome-steps">
        <article>
          <span class="step-number">01</span><Layers size={23} />
          <h3>Build your batch</h3>
          <p>
            Bring multiple documents together. Arrange pages and repair scans
            without modifying the source.
          </p>
        </article>
        <article>
          <span class="step-number">02</span><ScanLine size={23} />
          <h3>Recognize & review</h3>
          <p>
            Choose a processing profile. Follow uncertain regions, correct text,
            and approve each page.
          </p>
        </article>
        <article>
          <span class="step-number">03</span><Download size={23} />
          <h3>Export with confidence</h3>
          <p>
            Check page completeness, preserve corrections, and save searchable
            or editable output.
          </p>
        </article>
      </div>
      <div class="welcome-footer">
        <ShieldCheck size={14} /> Local processing · Automatic project recovery ·
        Original files preserved
      </div>
    </div>
  {:else}
    <nav class="mobile-workspace-nav" aria-label="Workspace views">
      <button class:active={mobileView === "pages"} aria-pressed={mobileView === "pages"} onclick={() => { mobileView = "pages"; pageRail = true; }}><Layers size={16} /> Pages <span>{project.pages.length}</span></button>
      <button class:active={mobileView === "image"} aria-pressed={mobileView === "image"} onclick={() => (mobileView = "image")}><Image size={16} /> Page image</button>
      <button class:active={mobileView === "tools"} aria-pressed={mobileView === "tools"} onclick={() => (mobileView = "tools")}><FileText size={16} /> Text & tools</button>
    </nav>
    <div class="work-area" class:rail-hidden={!pageRail} data-mobile-view={mobileView}>
      {#if pageRail || mobileView === "pages"}
        <aside class="page-rail" aria-label="Document pages">
          <div class="rail-heading">
            <strong>Pages <span>{project.pages.length}</span></strong><button
              class="icon-button"
              onclick={() => { pageRail = false; mobileView = "image"; }}
              aria-label="Hide page list"><PanelLeftClose size={16} /></button
            >
          </div>
          <div class="rail-controls">
            <div class="search-input">
              <Search size={13} /><input
                bind:value={pageQuery}
                placeholder="Find a document…"
                aria-label="Filter documents"
              />
            </div>
            <select bind:value={pageFilter} aria-label="Filter pages by status"
              ><option value="all">All pages</option><option value="review"
                >Needs review</option
              ><option value="pending">Pending</option><option value="failed"
                >Failed</option
              ><option value="approved">Approved</option><option
                value="excluded">Excluded</option
              ></select
            >
          </div>
          <div class="selection-row">
            <label
              ><input
                type="checkbox"
                checked={visiblePages.length > 0 &&
                  visiblePages.every((p) => checkedPages.includes(p.id))}
                onchange={(e) =>
                  (checkedPages = e.currentTarget.checked
                    ? [
                        ...new Set([
                          ...checkedPages,
                          ...visiblePages.map((p) => p.id),
                        ]),
                      ]
                    : checkedPages.filter(
                        (id) => !visiblePages.some((p) => p.id === id),
                      ))}
              /><span
                >{checkedPages.length
                  ? `${checkedPages.length} selected`
                  : "Select pages"}</span
              ></label
            >{#if checkedPages.length}<button
                class="text-button"
                onclick={() => (checkedPages = [])}>Clear</button
              >{/if}
          </div>
          <div class="page-list">
            {#each windowPages as page (page.id)}
              {@const number = pageNumbers.get(page.id) ?? 0}
              {@const issues = issueCounts.get(page.id) ?? 0}
              <div
                class="page-card"
                class:current={activePage?.id === page.id}
                class:excluded={page.excluded}
              >
                <input
                  class="page-checkbox"
                  type="checkbox"
                  checked={checkedPages.includes(page.id)}
                  onchange={() => togglePage(page.id)}
                  aria-label={`Select page ${number}`}
                />
                <button
                  class="page-card-main"
                  onclick={() => { selectPage(page.id); mobileView = "image"; }}
                  aria-label={`Page ${number}: ${page.name}`}
                  aria-current={activePage?.id === page.id ? "page" : undefined}
                >
                  <div class="thumbnail">
                    <img
                      src={client.imageUrl(page.imagePath)}
                      alt=""
                      loading="lazy"
                      decoding="async"
                    />{#if page.approved}<span class="thumb-approved"
                        ><Check size={11} /></span
                      >{/if}
                  </div>
                  <span class="page-card-info"
                    ><strong>Page {number}</strong><span
                      class="page-filename"
                      title={page.name}>{page.name}</span
                    ><span
                      class="page-state"
                      class:failed={page.status === "failed"}
                      class:approved={page.approved}
                      >{#if page.excluded}Excluded{:else if page.status === "running"}<LoaderCircle
                          size={10}
                          class="spin"
                        /> Recognizing{:else if page.status === "failed"}<AlertTriangle
                          size={10}
                        /> Failed{:else if page.approved}<Check size={11} /> Approved{:else if page.status === "pending"}<Clock3
                          size={10}
                        /> Pending{:else if issues}<span class="amber-dot"
                        ></span>{issues} to review{:else}<CircleCheck
                          size={10}
                        /> Recognized{/if}</span
                    ></span
                  >
                </button>
              </div>
            {/each}
            {#if visiblePages.length === 0}<p class="rail-empty">
                No pages match this filter.
              </p>{/if}
          </div>
          {#if visiblePages.length > pageWindowSize}
            <div class="page-window-controls">
              <button
                class="icon-button"
                aria-label="Previous group of pages"
                disabled={pageWindowStart === 0}
                onclick={() =>
                  (pageWindowStart = Math.max(
                    0,
                    pageWindowStart - pageWindowSize,
                  ))}><ChevronLeft size={14} /></button
              >
              <span
                >{pageWindowStart + 1}–{Math.min(
                  pageWindowStart + pageWindowSize,
                  visiblePages.length,
                )} of {visiblePages.length}</span
              >
              <button
                class="icon-button"
                aria-label="Next group of pages"
                disabled={pageWindowStart + pageWindowSize >=
                  visiblePages.length}
                onclick={() => (pageWindowStart += pageWindowSize)}
                ><ChevronRight size={14} /></button
              >
            </div>
          {/if}
          <div class="range-control">
            <label for="ocr-page-range">Select a range</label>
            <div>
              <input
                id="ocr-page-range"
                bind:value={pageRange}
                placeholder="1-4, 8"
                onkeydown={(e) => {
                  if (e.key === "Enter") applyPageRange();
                }}
              /><button
                class="icon-button"
                onclick={applyPageRange}
                aria-label="Apply page range"><Check size={14} /></button
              >
            </div>
          </div>
          <div class="rail-footer">
            <button
              class="icon-button"
              onclick={() =>
                activePage && change(movePage(project, activePage.id, -1))}
              disabled={locked || pageIndex < 1}
              aria-label="Move page up"
              title="Move page earlier"><ArrowUp size={15} /></button
            ><button
              class="icon-button"
              onclick={() =>
                activePage && change(movePage(project, activePage.id, 1))}
              disabled={locked || pageIndex === project.pages.length - 1}
              aria-label="Move page down"
              title="Move page later"><ArrowDown size={15} /></button
            ><button
              class="icon-button"
              onclick={() =>
                activePage &&
                change(
                  updatePage(project, activePage.id, {
                    excluded: !activePage.excluded,
                  }),
                )}
              disabled={locked || !activePage}
              aria-label={activePage?.excluded
                ? "Include page"
                : "Exclude page"}
              title={activePage?.excluded
                ? "Include in output"
                : "Exclude from output"}
              >{#if activePage?.excluded}<Eye size={15} />{:else}<EyeOff
                  size={15}
                />{/if}</button
            >
            <div class="toolbar-spacer"></div>
            <button
              class="icon-button danger"
              onclick={confirmRemovePage}
              disabled={locked || !activePage}
              aria-label="Remove page"
              title="Remove page"><Trash2 size={15} /></button
            >
          </div>
        </aside>
      {/if}

      <main class="document-pane" aria-label="Page image and regions">
        <div class="image-toolbar">
          {#if !pageRail}<button
              class="icon-button"
              onclick={() => { pageRail = true; mobileView = "pages"; }}
              aria-label="Show page list"><PanelLeftOpen size={16} /></button
            >{/if}
          <div class="tool-group">
            <select
              bind:value={drawMode}
              disabled={locked || showOriginal}
              aria-label="Region drawing tool"
              ><option value="select">Select region</option><option value="text"
                >Draw text region</option
              ><option value="table">Draw table region</option><option
                value="picture">Draw picture region</option
              ><option value="formula">Draw formula region</option><option
                value="recognize"
                disabled={!recognitionAvailable}>Recognize a zone</option
              ><option value="crop">Crop page</option></select
            >
          </div>
          <div class="tool-group">
            <button
              class="icon-button"
              class:active={drawMode === "recognize"}
              onclick={drawRecognitionArea}
              disabled={locked || !activePage || !recognitionAvailable}
              title="Re-OCR area: draw an area to recognize again, then compare before replacing text"
              aria-label="Re-OCR area"
              ><ScanLine size={16} /></button
            >
          </div>
          <div class="tool-group">
            <button
              class="icon-button"
              onclick={() => transform("rotate", { degrees: 270 })}
              disabled={locked}
              aria-label="Rotate page left"
              title="Rotate left"><RotateCcw size={16} /></button
            ><button
              class="icon-button"
              onclick={() => transform("rotate", { degrees: 90 })}
              disabled={locked}
              aria-label="Rotate page right"
              title="Rotate right"><RotateCw size={16} /></button
            ><button
              class="icon-button"
              class:active={drawMode === "crop"}
              onclick={() => {
                drawMode = "crop";
                showOriginal = false;
              }}
              disabled={locked}
              aria-label="Crop page"
              title="Crop page"><Crop size={16} /></button
            ><button
              class="icon-button"
              onclick={confirmSplitPage}
              disabled={locked}
              aria-label="Split book spread"
              title="Split book spread"><Scissors size={16} /></button
            >
          </div>
          <div class="toolbar-spacer"></div>
          <button
            class="icon-button"
            class:active={overlays}
            onclick={() => (overlays = !overlays)}
            title="Toggle region overlays"
            aria-label="Toggle region overlays"><Layers size={16} /></button
          ><button
            class="icon-button"
            class:active={showOriginal}
            onclick={() => { showOriginal = !showOriginal; perspective = false; }}
            aria-label="Original"
            title="Compare with the preserved original image"><FileImage size={16} /></button
          >
          {#if activePage && capabilities.photo_tools}
          <button class="icon-button" class:active={photoOpen} bind:this={photoToolsButton} onclick={showPhotoTools} disabled={locked} aria-label="Prepare photo"
            title={perspective ? "Prepare photo: set the four paper corners" : "Prepare photo: perspective, tilt, lighting"}><Camera size={16} /></button>
          {/if}
        </div>
        {#if activePage && capabilities.photo_tools}
          {#if photoOpen}<PhotoPreparation width={activePage.width} height={activePage.height} assessment={photoAssessment} assessing={busy === "Checking photo quality"} disabled={locked}
            {perspective} quad={photoQuad} angle={photoAngle} canWhiten={capabilities.ocr} onassess={assessPhoto} onperspective={setPerspective} onquad={(quad) => (photoQuad = quad)}
            onangle={(angle) => (photoAngle = angle)} onapplyperspective={applyPerspective} onapplyangle={() => transform("deskew", { angle: photoAngle })} onwhiten={flattenShadows}
            onclose={() => { photoOpen = false; perspective = false; }} />{/if}
          {#if photoOpen && perspective}
            {@const cornerError = photoQuadError(photoQuad, activePage.width, activePage.height)}
            <div class="photo-focus-controls" role="region" aria-label="Photo corner editing">
              <div class="photo-focus-actions">
                <button class="button" bind:this={photoFocusBack} onclick={() => setPerspective(false)}><ChevronLeft size={16} />Back to photo tools</button>
                <button class="button primary" onclick={applyPerspective} disabled={locked || !!cornerError}><Check size={16} />Apply perspective</button>
              </div>
              {#if cornerError}<p class="photo-focus-error" role="status">{cornerError}</p>{/if}
              <details class="photo-focus-coordinates" bind:open={photoFocusCoordinatesOpen}>
                <summary>Exact corner positions</summary>
                <div class="photo-focus-coordinate-grid">
                  {#each photoQuad as point, index}
                    <fieldset>
                      <legend>{CORNER_NAMES[index]}</legend>
                      <label>X<input type="number" min="0" max={activePage.width - 1} step="1" value={point.x} aria-label={`${CORNER_NAMES[index]} corner X`} oninput={(event) => setPhotoCoordinate(index, "x", event.currentTarget.valueAsNumber)} disabled={locked} /></label>
                      <label>Y<input type="number" min="0" max={activePage.height - 1} step="1" value={point.y} aria-label={`${CORNER_NAMES[index]} corner Y`} oninput={(event) => setPhotoCoordinate(index, "y", event.currentTarget.valueAsNumber)} disabled={locked} /></label>
                    </fieldset>
                  {/each}
                </div>
              </details>
            </div>
          {/if}
        {/if}
        {#if activePage}
          <PageCanvas
            page={activePage}
            imageUrl={client.imageUrl}
            selectedId={selectedRegionId}
            sourceWord={wordSource}
            threshold={project.confidenceThreshold}
            mode={drawMode}
            photoQuad={perspective ? photoQuad : null}
            onphotoquad={(quad) => (photoQuad = quad)}
            {overlays}
            original={showOriginal}
            {zoom}
            {fit}
            disabled={locked}
            onselect={selectRegion}
            onrectangle={onRectangle}
            ongeometry={(id, rect) =>
              safe(() =>
                change(updateRegion(project, activePage.id, id, rect)),
              )}
          />
        {/if}
        <div class="image-footer">
          <button
            class="icon-button page-navigation"
            onclick={() => gotoPage(-1)}
            disabled={pageIndex <= 0}
            aria-label="Previous page"><ChevronLeft size={17} /></button
          ><span class="page-counter page-navigation"
            >{pageIndex + 1} <span>/ {project.pages.length}</span></span
          ><button
            class="icon-button page-navigation"
            onclick={() => gotoPage(1)}
            disabled={pageIndex >= project.pages.length - 1}
            aria-label="Next page"><ChevronRight size={17} /></button
          >
          <div class="toolbar-spacer"></div>
          <button
            class="icon-button"
            onclick={() => {
              zoom = Math.max(0.1, zoom - 0.25);
              fit = "custom";
            }}
            aria-label="Zoom out"><ZoomOut size={16} /></button
          ><select
            value={fit === "custom" ? String(zoom) : fit}
            onchange={(e) => {
              const v = e.currentTarget.value;
              if (v === "page" || v === "width") fit = v;
              else {
                fit = "custom";
                zoom = Number(v);
              }
            }}
            aria-label="Page zoom"
            ><option value="page">Fit page</option><option value="width"
              >Fit width</option
            ><option value="0.5">50%</option><option value="1">100%</option
            ><option value="1.5">150%</option><option value="2">200%</option
            ><option value="3">300%</option
            >{#if fit === "custom" && ![0.5, 1, 1.5, 2, 3].includes(zoom)}<option
                value={zoom}>{Math.round(zoom * 100)}%</option
              >{/if}</select
          ><button
            class="icon-button"
            onclick={() => {
              zoom = Math.min(6, zoom + 0.25);
              fit = "custom";
            }}
            aria-label="Zoom in"><ZoomIn size={16} /></button
          ><button
            class="icon-button"
            onclick={() => (fit = "page")}
            aria-label="Fit page"
            title="Fit page"><Maximize2 size={15} /></button
          >
        </div>
      </main>

      <aside class="inspector" aria-label="Recognition and review inspector">
        <div
          class="inspector-tabs"
          role="tablist"
          aria-label="Workspace panels"
        >
          <button
            role="tab"
            aria-selected={inspector === "review"}
            onclick={() => openInspector("review")}
            class:active={inspector === "review"}
            ><CheckCheck size={15} /> Review</button
          ><button
            role="tab"
            aria-selected={inspector === "recognition"}
            onclick={() => openInspector("recognition")}
            class:active={inspector === "recognition"}
            ><SlidersHorizontal size={15} /> Recognize</button
          ><button
            role="tab"
            aria-selected={inspector === "extract"}
            onclick={() => openInspector("extract")}
            class:active={inspector === "extract"}
            ><Braces size={15} /> Extract</button
          ><button
            role="tab"
            aria-selected={inspector === "export"}
            onclick={() => openInspector("export")}
            class:active={inspector === "export"}
            ><Download size={15} /> Export</button
          >
        </div>
        {#if inspector === "review"}
          <div class="review-bar">
            <div class="review-view-tabs" role="group" aria-label="Text view">
              <button
                class:active={textView === "document"}
                onclick={() => { currentMatch = null; wordInspection = null; textView = "document"; }}
                title="The page text as one document, words coloured by confidence">Document</button
              ><button
                class:active={textView === "regions"}
                onclick={() => (textView = "regions")}
                title="Region by region, with word scores">Regions</button
              ><button
                class:active={textView === "text"}
                onclick={() => { currentMatch = null; textView = "text"; }}
                title="Passage tools: split, merge, repair and re-recognize passages">Passages</button
              ><button
                class:active={textView === "structure"}
                onclick={() => { currentMatch = null; wordInspection = null; editGroup = null; textView = "structure"; }}
                title="Headings, lists, columns and tables">Structure</button
              >
            </div>
            <div class="toolbar-spacer"></div>
            {#if recognitionWarnings.length}
              <details class="recognition-notes">
                <summary title="Recognition notes" aria-label={`${recognitionWarnings.length} recognition notes`}
                  ><AlertTriangle size={14} />{recognitionWarnings.length}</summary
                >
                <ul>
                  {#each recognitionWarnings as warning}<li>{warning}</li>{/each}
                </ul>
              </details>
            {/if}
            <button
              class="icon-button"
              onclick={reviewNext}
              disabled={!reviewIssues.length}
              aria-label="Next issue"
              title={`Next issue (${pageIssues.length} on this page, ${reviewIssues.length} in the project)`}
              ><ChevronRight size={16} /></button
            ><button
              class="icon-button"
              onclick={openPageTextDraft}
              disabled={locked || !activePage || activePage.excluded || !activePage.regions.some((item) => !item.excluded && ["text", "title", "header", "footer"].includes(item.kind)) || (!!project.pageTextDraft && project.pageTextDraft.page.id !== activePage.id)}
              aria-label="Edit page text"
              title={project.pageTextDraft && project.pageTextDraft.page.id !== activePage?.id ? "Resume or discard the existing draft before starting another page" : "Edit page text: change across passages in a saved draft, then review before applying"}
              ><FileText size={16} /></button
            ><button
              class="icon-button"
              class:active={proofreadingOpen}
              onclick={toggleProofreading}
              aria-label="Vocabulary and rules"
              title="Vocabulary and correction rules"
              ><BookOpen size={16} /></button
            ><button
              class="icon-button"
              class:active={searchOpen}
              onclick={() => (searchOpen = !searchOpen)}
              aria-label="Find and replace"
              title="Find and replace (Ctrl/Cmd F)"><Search size={16} /></button
            >
          </div>
          {#if proofreadingOpen}
            <ProofreadingPanel
              projectId={project.id}
              projectData={projectProofreading}
              userData={userProofreading}
              language={projectProofreading.language}
              userLoadState={userProofreadingLoad}
              userLoadError={userProofreadingError}
              userSaveStatus={userProofreadingStatus}
              userWritable={userProofreadingWritable}
              disabled={locked}
              hasPage={!!activePage}
              hasRegion={!!region && !region.excluded && !["picture", "formula", "table"].includes(region.kind)}
              selectedPageCount={checkedPages.length}
              draftRule={correctionDraft}
              onproject={setProjectProofreading}
              onuser={setUserProofreading}
              onlanguage={setProofreadingLanguage}
              onreload={loadUserProofreading}
              onretry={async () => { await userProofreadingWriter.retry(); }}
              onexport={exportProofreading}
              onpreview={previewRule}
              onclose={() => { proofreadingOpen = false; correctionDraft = null; }}
            />
          {:else}
          {#if searchOpen}<div class="find-panel">
              <div class="find-row">
                <input
                  bind:value={query}
                  placeholder="Find in project…"
                  aria-label="Find text"
                  onkeydown={(e) => {
                    if (e.key === "Enter") {
                      e.preventDefault();
                      void findNext(e.shiftKey ? -1 : 1);
                    }
                  }}
                /><span>{matches.length}</span><button
                  class="icon-button"
                  onclick={() => findNext(-1)}
                  disabled={!matches.length}
                  aria-label="Previous match"><ChevronLeft size={14} /></button
                ><button
                  class="icon-button"
                  onclick={() => findNext()}
                  disabled={!matches.length}
                  aria-label="Next match"><ChevronRight size={14} /></button
                >
              </div>
              <div class="find-row">
                <input
                  bind:value={replacement}
                  placeholder="Replace with…"
                  aria-label="Replacement text"
                /><button
                  class="button small"
                  onclick={replaceOne}
                  disabled={locked || !currentMatch}>Replace</button
                ><button
                  class="button small"
                  onclick={() =>
                    safe(() => {
                      change(
                        replaceMatches(project, query, replacement, {
                          caseSensitive: matchCase,
                        }),
                      );
                      currentMatch = null;
                    })}
                  disabled={locked || !matches.length}>All</button
                >
              </div>
              <label class="check-label"
                ><input type="checkbox" bind:checked={matchCase} /> Match case
                <span class="muted">· Literal text, included pages</span></label
              >
            </div>{/if}
          {#if textView === "regions"}<label class="check-label needs-attention"
              ><input type="checkbox" bind:checked={onlyIssues} /> Needs attention</label
            >{/if}
          {#if region && !showOriginal && textView !== "document"}<div class="source-loupe">
              <div class="loupe-label">
                <ScanText size={12} /> SOURCE DETAIL
                <span title={wordSource?.text}
                  >{#if wordSource}Word: {wordSource.text}{:else}Region {(activePage?.regions.findIndex(
                    (r) => r.id === region.id,
                  ) ?? 0) + 1}{/if}</span
                >
              </div>
              <svg
                viewBox={loupeViewBox}
                data-word-source-loupe={wordSource ? "true" : undefined}
                role="img"
                aria-label="Magnified selected source text"
                ><image
                  href={client.imageUrl(activePage.imagePath)}
                  width={activePage.width}
                  height={activePage.height}
                /></svg
              >
            </div>{/if}
          <div class="region-list" bind:this={editorPane}>
            {#if activePage?.provenance.structureHistory}<PreviousStructure value={activePage.provenance.structureHistory} imageUrl={client.imageUrl} />{/if}
            {#if earlierSourceComponents.length}<details class="recognition-notes" data-earlier-source-components>
                <summary>Earlier source components</summary>
                <p class="muted">These records describe earlier page images and remain inspectable before recognizing the current image.</p>
                <PassageSources components={earlierSourceComponents} imageUrl={client.imageUrl} />
              </details>{/if}
            {#if activePage?.status === "failed"}<div
                class="panel-message warning"
              >
                <AlertTriangle size={24} />
                <h3>Recognition needs another try</h3>
                <p>{activePage.error}</p>
                <button
                  class="button"
                  onclick={() => recognize("current")}
                  disabled={locked || !recognitionAvailable}
                  ><RefreshCw size={14} /> Retry this page</button
                >
              </div>
            {:else if activePage?.status === "pending"}<div
                class="panel-message"
              >
                <ScanText size={32} />
                <h3>Ready to recognize</h3>
                <p>
                  Choose a profile and recognize this page. You can also draw a
                  region and enter text manually.
                </p>
                <button
                  class="button primary"
                  onclick={() => recognize("current")}
                  disabled={locked || !recognitionAvailable}
                  ><ScanLine size={15} /> Recognize page</button
                >
              </div>
            {:else if textView === "document"}
              <DocumentText
                page={activePage}
                threshold={project.confidenceThreshold}
                selectedId={selectedRegionId}
                disabled={locked}
                engine={recognitionEngine}
                onselect={(id) => { currentMatch = null; wordInspection = null; selectedRegionId = id; }}
                onedit={editText}
                onblur={() => (editGroup = null)}
              />
            {:else if textView === "structure"}
              <StructurePanel
                {project}
                pageId={activePage.id}
                selectedId={selectedRegionId}
                disabled={locked}
                imageUrl={client.imageUrl}
                onselect={(id) => {
                  currentMatch = null;
                  wordInspection = null;
                  editGroup = null;
                  selectedRegionId = id;
                }}
                onchange={(next, message) => {
                  change(next);
                  currentMatch = null;
                  wordInspection = null;
                  error = "";
                  notice = message;
                }}
                onedit={editText}
                onblur={() => (editGroup = null)}
              />
            {:else if textView === "text"}<div class="reading-order">
                <PageTextEditor
                  page={activePage}
                  selectedId={selectedRegionId}
                  disabled={locked}
                  ocrAvailable={recognitionAvailable}
                  onfocus={(id) => {
                    currentMatch = null;
                    wordInspection = null;
                    selectedRegionId = id;
                  }}
                  onedit={editText}
                  onblur={() => (editGroup = null)}
                  onrecognize={(r) => recognizeZone(r, activePage.id)}
                  onsplit={(r, caret) => openPassageAction("split", r, caret)}
                  onmerge={(r) => openPassageAction("merge", r)}
                  onrepair={(r) => openPassageAction("repair", r)}
                  imageUrl={client.imageUrl}
                />
                <button
                  class="button small"
                  onclick={() => copy(pageText)}
                  disabled={!pageText}><Copy size={13} /> Copy page text</button
                >
              </div>
            {:else if visibleRegions.length === 0}<div class="panel-message">
                <CircleCheck size={28} />
                <h3>{onlyIssues ? "No flagged regions" : "No text regions"}</h3>
                <p>
                  {onlyIssues
                    ? "Review the page image and approve it when ready."
                    : "Check whether this is a blank or image-only page, or draw a text region to add text."}
                </p>
              </div>
            {:else}
              {#each visibleRegions as r (r.id)}
                {@const regionIndex = activePage.regions.findIndex(
                  (item) => item.id === r.id,
                )}
                <article
                  class="region-card"
                  data-region-id={r.id}
                  class:selected={r.id === selectedRegionId}
                  class:attention={issueRegionIds.has(r.id)}
                  class:excluded={r.excluded}
                >
                  <div class="region-card-heading">
                    <button
                      class="region-number"
                      onclick={() => selectRegion(r.id)}
                      aria-label={`Select region ${regionIndex + 1}`}
                      >{regionIndex + 1}</button
                    ><button
                      class="region-kind"
                      onclick={() => selectRegion(r.id)}>{r.kind}</button
                    ><span
                      class="confidence"
                      class:unknown={r.confidenceBasis !== "recognition" || r.confidence === null}
                      title={r.confidenceBasis === "recognition"
                        ? r.text !== r.originalText
                          ? "Original OCR region score. Manual corrections do not produce a new recognition score; this is a review signal, not calibrated accuracy."
                          : "Region recognition score; a review signal, not calibrated accuracy. Word scores are shown separately when available."
                        : r.confidenceBasis === "detection"
                          ? "Detection score only; text confidence is unknown"
                          : "Recognition confidence unavailable"}
                      >{r.confidenceBasis === "recognition" &&
                      r.confidence !== null
                        ? `Region ${Math.round(r.confidence * 100)}%`
                        : r.confidenceBasis === "manual"
                          ? "Manual region"
                          : "Region: unscored"}</span
                    >{#if r.review === "approved"}<span
                        class="region-approved"
                        title="Approved"><Check size={14} /></span
                      >{:else if r.review === "corrected"}<span
                        class="edited-badge">Edited</span
                      >{/if}
                    {#if r.sharedSourceGroups?.length}<span
                        class="shared-source-badge"
                        data-shared-source-groups={r.sharedSourceGroups.join(" ")}
                        title="Passages in this source family may share image coverage. Spatial exports require their source areas to stop overlapping, or the passages to be merged again."
                      >Shared source area</span>{/if}
                  </div>
                  {#if hasAlignedCharacterConfidence(r)}<div
                      class="character-evidence"
                      aria-label="Original text with character confidence"
                    >
                      <span class="evidence-label">OCR</span
                      >{#each Array.from(r.originalText) as ch, index}<span
                          title={`Original character score: ${Math.round(r.char_conf[index] * 100)}%`}
                          class:uncertain={(r.char_conf[index] ?? 1) <
                            project.confidenceThreshold}>{ch}</span
                        >{/each}
                    </div>{/if}
                  <textarea
                    data-region-text
                    aria-label={`Text for region ${regionIndex + 1}`}
                    value={r.text}
                    oninput={(e) => editText(r, e.currentTarget.value)}
                    onfocus={() => {
                      wordInspection = null;
                      selectedRegionId = r.id;
                    }}
                    onblur={() => (editGroup = null)}
                    disabled={locked || r.excluded}
                    rows={Math.max(
                      2,
                      Math.min(
                        8,
                        r.text.split("\n").length +
                          Math.floor(r.text.length / 48),
                      ),
                    )}
                    spellcheck="true"
                    dir="auto"
                  ></textarea>
                  {#if r.id === selectedRegionId}
                    <div class="region-edit-actions">
                      <button
                        class="text-button"
                        onclick={() => editPassage(r)}
                        disabled={locked || r.excluded || r.kind === "picture"}
                        title="Open the page text editor for full sentences and paragraphs"
                        ><FileText size={13} /> Edit passage</button
                      >
                      <button
                        class="text-button"
                        onclick={() => recognizeZone(r, activePage.id)}
                        disabled={locked || r.excluded || !recognitionAvailable}
                        title="Recognize this source region again and compare before replacing text"
                        ><ScanLine size={13} /> Re-OCR region</button
                      >
                      <button
                        class="text-button"
                        onclick={() => openPassageAction("split", r, readRegionCaret(r.id))}
                        disabled={locked || !selectedPassageEligibility?.splitAllowed}
                        title="Place the caret where the passage should split, then choose the source coverage"
                        ><Scissors size={13} /> Split passage</button
                      >
                      <button
                        class="text-button"
                        onclick={() => openPassageAction("merge", r)}
                        disabled={locked || !selectedPassageEligibility?.nextMergeId}
                        title={selectedPassageEligibility?.nextMergeId ? "Preview a merge with the immediately following passage" : "Merge needs the immediately following included passage of the same kind"}
                        ><Layers size={13} /> Merge next passage</button
                      >
                      <button
                        class="text-button"
                        onclick={() => openPassageAction("repair", r)}
                        disabled={locked || activePage.excluded || r.excluded || !r.text.trim() || !["text", "title", "header", "footer"].includes(r.kind)}
                        title="Review proposed whitespace, line-break, or hyphenation changes"
                        ><WandSparkles size={13} /> Repair passage text</button
                      >
                    </div>
                    <WordReview
                      region={r}
                      threshold={project.confidenceThreshold}
                      disabled={locked || r.excluded}
                      onselect={(word) => selectWord(r, word)}
                      oninspect={(word, source) => inspectWord(r, word, source)}
                    />
                    {#if proofreadingSelection && proofreadingSelection.regionId === r.id}
                      <WordSuggestions
                        selection={proofreadingSelection}
                        language={projectProofreading.language}
                        suggestions={proofreadingSuggestions.items}
                        errorMessage={proofreadingSuggestions.error}
                        ignored={proofreadingIgnored}
                        disabled={locked}
                        ready={proofreadingReady}
                        userEditable={userProofreadingWritable}
                        onlanguage={setProofreadingLanguage}
                        onadd={addSelectedVocabulary}
                        onignore={ignoreSelectedSuggestion}
                        onpreview={previewSuggestion}
                        onremember={rememberCorrection}
                      />
                    {/if}
                    {#if r.sourceComponents?.length}<PassageSources components={r.sourceComponents} imageUrl={client.imageUrl} />{/if}
                  {/if}
                  {#if r.id === selectedRegionId}<div class="region-actions">
                      <button
                        class="text-button"
                        onclick={() => {
                          safe(() =>
                            change(
                              updateRegion(project, activePage.id, r.id, {
                                review:
                                  r.review === "approved"
                                    ? "unreviewed"
                                    : "approved",
                              }),
                            ),
                          );
                        }}
                        disabled={locked || r.excluded}
                        ><Check size={13} />{r.review === "approved"
                          ? "Reopen"
                          : "Accept region"}</button
                      >
                      <div class="toolbar-spacer"></div>
                      <button
                        class="icon-button"
                        onclick={() =>
                          safe(() =>
                            change(
                              moveRegion(project, activePage.id, r.id, -1),
                            ),
                          )}
                        disabled={locked || regionIndex === 0}
                        title="Read earlier"
                        aria-label="Move region earlier"
                        ><ArrowUp size={13} /></button
                      ><button
                        class="icon-button"
                        onclick={() =>
                          safe(() =>
                            change(moveRegion(project, activePage.id, r.id, 1)),
                          )}
                        disabled={locked ||
                          regionIndex === activePage.regions.length - 1}
                        title="Read later"
                        aria-label="Move region later"
                        ><ArrowDown size={13} /></button
                      ><button
                        class="icon-button"
                        onclick={() => (geometryOpen = !geometryOpen)}
                        title="Region properties"
                        aria-label="Region properties"
                        ><SlidersHorizontal size={13} /></button
                      >
                    </div>{/if}
                </article>
              {/each}
            {/if}
          </div>
          {#if region && geometryOpen}<div class="region-properties">
              <div class="section-heading">
                <strong>Region properties</strong><button
                  class="icon-button"
                  onclick={() => (geometryOpen = false)}
                  aria-label="Close region properties"><X size={13} /></button
                >
              </div>
              <div class="field-grid">
                <label
                  >Type<select
                    value={region.kind}
                    onchange={(e) =>
                      patchRegion({
                        kind: e.currentTarget.value as RegionKind,
                      })}
                    disabled={locked}
                    >{#each regionsKinds as kind}<option value={kind}
                        >{kind}</option
                      >{/each}</select
                  ></label
                >{#each ["x", "y", "w", "h"] as key}<label
                    >{(
                      {
                        x: "Left",
                        y: "Top",
                        w: "Width",
                        h: "Height",
                      } as Record<string, string>
                    )[key]}<input
                      type="number"
                      min="0"
                      step="1"
                      value={Math.round(region[key as "x" | "y" | "w" | "h"])}
                      onchange={(e) =>
                        patchRegion({ [key]: Number(e.currentTarget.value) })}
                      disabled={locked}
                    /></label
                  >{/each}
              </div>
              <div class="region-actions">
                <label class="check-label"
                  ><input
                    type="checkbox"
                    checked={region.excluded}
                    onchange={(e) =>
                      patchRegion({ excluded: e.currentTarget.checked })}
                    disabled={locked}
                  /> Exclude from text</label
                >
                <div class="toolbar-spacer"></div>
                <button
                  class="text-button danger"
                  onclick={() =>
                    safe(() => {
                      change(deleteRegion(project, activePage.id, region.id));
                      selectedRegionId = null;
                    })}
                  disabled={locked}><Trash2 size={13} /> Remove</button
                >
              </div>
              {#if region.text !== region.originalText}<details>
                  <summary>{region.confidenceBasis === "manual" ? "Before later edits" : "Original recognition"}</summary>
                  <p class="original-text">
                    {region.originalText || "(Manually created region)"}
                  </p>
                  <button
                    class="text-button"
                    onclick={() => patchRegion({ text: region.originalText })}
                    disabled={locked}>{region.confidenceBasis === "manual" ? "Restore creation text" : "Restore original text"}</button
                  >
                </details>{/if}
            </div>{/if}
          <div class="approval-bar">
            <span
              >{#if activePage?.approved}<CircleCheck size={15} />{/if}</span
            ><button
              class="button"
              class:approved-button={activePage?.approved}
              onclick={approveCurrent}
              disabled={locked || activePage?.status !== "recognized"}
              ><CheckCheck size={16} />{activePage?.approved
                ? "Page approved"
                : "Approve page"}</button
            ><button
              class="button subtle"
              onclick={approveRegion}
              disabled={locked || !region || region.excluded}
              title="Approve this region and go to the next issue"
              >Accept & next <ChevronRight size={14} /></button
            >
          </div>
          {/if}
        {:else if inspector === "recognition"}
          <div class="panel-scroll settings-panel">
            <div class="panel-intro">
              <span class="eyebrow">RECOGNITION PIPELINE</span>
              <h2>The right treatment for every page</h2>
              <p>
                Profiles combine CrispEmbed recognition, scan restoration, and
                document layout tools. Changes apply to the next run.
              </p>
            </div>
            {#if !capabilities.ocr}<div class="inline-warning">
                <AlertTriangle size={16} />{capabilities.reason ??
                  "Recognition is unavailable in this build."}
              </div>{:else if !recognitionAvailable}<div class="inline-warning">
                <AlertTriangle size={16} />The selected engine is unavailable.
                Choose an installed engine below to recognize this page.
              </div>{/if}
            <label class="field-label"
              >Processing profile<select
                value={project.profileId}
                onchange={(e) => chooseProfile(e.currentTarget.value)}
                disabled={locked}
                >{#each PROFILES as p}<option value={p.id}>{p.name}</option
                  >{/each}</select
              ><small>{profile.description}</small></label
            >
            <div class="field-grid two">
              <label class="field-label"
                >Recognition engine<select
                  value={engine}
                  onchange={(e) => setSetting("engine", e.currentTarget.value)}
                  disabled={locked}
                  ><option value="auto" disabled={!capabilities.crispembed}>Automatic pipeline</option
                  >{#each capabilities.engines.filter((e) => e !== "auto") as e}<option
                      value={e}>{engineNames[e] ?? e}</option
                    >{/each}
                  {#if engine === "system_tesseract" && !capabilities.engines.includes(engine)}
                    <option value="system_tesseract" disabled>System Tesseract · unavailable</option>
                  {/if}</select
                ></label
              ><label class="field-label"
                >Document language<select
                  value={language}
                  onchange={(e) =>
                    setSetting("language", e.currentTarget.value)}
                  disabled={locked}
                  >{#if engine === "system_tesseract"}
                    <option value="auto">English (eng) default</option>
                    {#if language !== "auto" && !systemLanguages.includes(language)}
                      <option value={language}>{language} (configured)</option>
                    {/if}
                    {#each systemLanguages as code}<option value={code}>{languageNames[code] ?? code} ({code})</option>{/each}
                  {:else}
                    <option value="auto">Model default</option>
                    {#each Object.entries(languageNames) as [code, name]}<option value={code}>{name}</option>{/each}
                  {/if}</select
                ></label
              >
            </div>
            {#if engine === "system_tesseract"}<p class="field-hint" data-system-tesseract-info>
              System Tesseract{capabilities.system_tesseract?.version ? ` ${capabilities.system_tesseract.version}` : ""}
              uses the current working image and supplies word scores and source
              locations. Apply scan preparation first if needed. Pipeline cleanup,
              layout, and the Qwen2 VL CPU budget are not applied during this step.
              Choose an installed language; the default is English, without
              automatic language detection.
            </p>
              {#if language === "auto" && capabilities.system_tesseract?.available && !systemLanguages.includes("eng")}<p class="field-hint">
                English is not installed. Select one of the installed languages above.
              </p>{/if}
            {:else}<p class="field-hint">
              Language selects the language model for Tesseract LSTM. Other
              engines support the languages learned by their selected model;
              supported vision engines also receive a language hint.
            </p>{/if}
            {#if capabilities.system_tesseract && !capabilities.system_tesseract.available}<p class="field-hint">
              System Tesseract word evidence is unavailable:
              {capabilities.system_tesseract.reason ?? "Install Tesseract and a recognition language pack to use this engine."}
            </p>{/if}
            {@render mobileModelSetup()}
            {#if engine === "qwen2vl" && capabilities.cpu_budget}<label
                class="setting-toggle"
                ><span
                  ><strong>Low-memory CPU profile</strong><small
                    >Runs Qwen2 VL in an isolated CPU process with the vision
                    input limited to {(capabilities.cpu_budget_pixels ?? 262144).toLocaleString()}
                    pixels. Uses several GB of RAM and reloads the model on each
                    run; fine print may be missed, so review the result.</small
                  ></span
                ><input
                  type="checkbox"
                  checked={settings.cpuPixelBudget === true}
                  onchange={(e) =>
                    setSetting("cpuPixelBudget", e.currentTarget.checked)}
                  disabled={locked}
                /></label
              >{/if}
            <div class="settings-section">
              <h3><WandSparkles size={15} /> Scan preparation</h3>
              {#each [{ key: "cleanup_enabled", label: "Deskew & clean", hint: "Correct tilt and clean the background." }, { key: "denoise", label: "Learned denoise", hint: "Remove noise with NAFNet." }, { key: "dewarp", label: "Straighten curved pages", hint: "Recover text near book spines and folds." }, { key: "restore", label: "Image restoration", hint: "Repair the scan with the selected model and task." }, { key: "sr", label: "Upscale small scans", hint: "Super-resolve low-resolution input." }] as option}<label
                  class="setting-toggle"
                  ><span
                    ><strong>{option.label}</strong><small>{option.hint}</small
                    ></span
                  ><input
                    type="checkbox"
                    checked={!!config[option.key]}
                    onchange={(e) =>
                      setConfig(option.key, e.currentTarget.checked)}
                    disabled={locked || !capabilities.crispembed}
                  /></label
                >{/each}
              <div class="field-grid two">
                <label class="field-label"
                  >Restoration<select
                    value={String(config.restore_engine ?? "restormer")}
                    onchange={(e) =>
                      setConfig("restore_engine", e.currentTarget.value)}
                    disabled={locked || !capabilities.crispembed || !config.restore}
                    ><option value="restormer">Restormer</option><option
                      value="scunet">SCUNet</option
                    ><option value="instructir">InstructIR</option></select
                  ></label
                ><label class="field-label"
                  >Super-resolution<select
                    value={String(config.sr_engine ?? "pan")}
                    onchange={(e) =>
                      setConfig("sr_engine", e.currentTarget.value)}
                    disabled={locked || !capabilities.crispembed || !config.sr}
                    ><option value="pan">PAN · fast</option><option
                      value="esrgan">Real-ESRGAN · detail</option
                    ><option value="safmn">SAFMN · compact</option></select
                  ></label
                >
              </div>
              {#if config.restore_engine === "instructir"}
                <label class="field-label"
                  >Restoration task
                  <select
                    value={String(config.restore_task ?? "denoise")}
                    onchange={(e) =>
                      setConfig("restore_task", e.currentTarget.value)}
                    disabled={locked || !capabilities.crispembed || !config.restore}
                  >
                    <option value="denoise">Remove noise</option>
                    <option value="deblur">Reduce blur</option>
                    <option value="dehaze">Remove haze</option>
                    <option value="derain">Remove rain</option>
                    <option value="super_resolution">Improve resolution</option>
                    <option value="low_light">Recover low-light scans</option>
                    <option value="enhance">Enhance detail</option>
                  </select>
                </label>
              {/if}
              <button
                class="button full"
                onclick={() => prepareImage()}
                disabled={locked || !activePage || !capabilities.crispembed}
                ><WandSparkles size={15} /> Apply preparation to this page</button
              >
              <p class="field-hint">
                {#if engine === "system_tesseract"}Prepare the page before running System Tesseract.{:else}Recognition also applies enabled preparation.{/if} The prepared image
                becomes the review and export image. Undo restores the previous
                result.
              </p>
            </div>
            <div class="settings-section">
              <h3><Columns2 size={15} /> Layout & reading order</h3>
              <label class="setting-toggle"
                ><span
                  ><strong>Analyze document layout</strong><small
                    >Find columns, headings, tables, and formulas.</small
                  ></span
                ><input
                  type="checkbox"
                  checked={!!config.layout}
                  onchange={(e) => setConfig("layout", e.currentTarget.checked)}
                  disabled={locked || !capabilities.layout}
                /></label
              ><label class="setting-toggle"
                ><span
                  ><strong>Exclude running headers & footers</strong><small
                    >Keep repeating marginal text out of the output.</small
                  ></span
                ><input
                  type="checkbox"
                  checked={!!config.drop_headers_footers}
                  onchange={(e) =>
                    setConfig("drop_headers_footers", e.currentTarget.checked)}
                  disabled={locked || !config.layout}
                /></label
              ><label class="field-label"
                >Layout engine<select
                  value={String(config.layout_engine ?? "rtdetr")}
                  onchange={(e) =>
                    setConfig("layout_engine", e.currentTarget.value)}
                  disabled={locked || !config.layout}
                  ><option value="rtdetr">Semantic document layout</option
                  ><option value="cc"
                    >Connected components · no model download</option
                  ></select
                ></label
              >
            </div>
            <div class="settings-section">
              <h3><ShieldCheck size={15} /> Review threshold</h3>
              <label class="threshold-label"
                >Flag recognition scores below <strong
                  >{Math.round(project.confidenceThreshold * 100)}%</strong
                ><input
                  type="range"
                  min="0"
                  max="1"
                  step="0.01"
                  value={project.confidenceThreshold}
                  onchange={(e) =>
                    change({
                      ...project,
                      confidenceThreshold: Number(e.currentTarget.value),
                      updatedAt: new Date().toISOString(),
                    })}
                  disabled={locked}
                /></label
              >
              <p class="field-hint">
                Engine scores guide review; they are not a guarantee of accuracy.
                Review uses word evidence when available and region scores as a
                fallback. Missing usable recognition evidence and corrected text
                need a visual check.
              </p>
            </div>
            <details class="advanced-settings">
              <summary>Models & advanced pipeline</summary>
              <p class="field-hint">
                Use installed model paths or CrispEmbed registry names. Model
                availability is checked when the pipeline runs.
              </p>
              {#each [{ key: "det_model", label: "Detection model" }, { key: "rec_model", label: "Recognition model" }, { key: "vlm_ocr_model", label: "Escalation model" }, { key: "punct_model", label: "Punctuation restoration" }, { key: "truecase_model", label: "Truecase model" }, { key: "lid_model", label: "Language identification model" }] as field}<label
                  class="field-label"
                  >{field.label}<input
                    value={String(config[field.key] ?? "")}
                    placeholder="Use engine default"
                    disabled={locked}
                    onchange={(e) =>
                      setConfig(
                        field.key,
                        e.currentTarget.value.trim() || null,
                      )}
                  /></label
                >{/each}<label class="field-label"
                >Pipeline configuration<textarea
                  class="config-editor"
                  bind:value={advancedConfig}
                  placeholder={'{"stages": []}'}
                  rows="5"
                  spellcheck="false"
                  disabled={locked}
                ></textarea></label
              >{#if configError}<p class="text-error">
                  {configError}
                </p>{/if}<button
                class="button small"
                onclick={() =>
                  (advancedConfig = JSON.stringify(config, null, 2))}
                disabled={locked}>Inspect current configuration</button
              ><button
                class="button small"
                onclick={applyAdvanced}
                disabled={locked || !advancedConfig.trim()}
                >Apply configuration</button
              >
            </details>
            <div class="settings-section">
              <h3>Batch actions</h3>
              <div class="button-grid">
                <button
                  class="button"
                  onclick={() => recognize("all")}
                  disabled={locked || !recognitionAvailable}
                  ><Play size={14} /> Recognize all</button
                ><button
                  class="button"
                  onclick={() => recognize("failed")}
                  disabled={locked || !metrics.failed || !recognitionAvailable}
                  ><RefreshCw size={14} /> Retry failed</button
                >
              </div>
              <p class="field-hint">
                Pause and Stop take effect after the current page. Completed
                results are saved as the queue progresses.
              </p>
            </div>
            {#if activePage?.provenance && Object.keys(activePage.provenance).length}<details
                class="advanced-settings"
              >
                <summary>Current page provenance</summary>
                <pre class="provenance">{JSON.stringify(
                    activePage.provenance,
                    null,
                    2,
                  )}</pre>
              </details>{/if}
          </div>
        {:else if inspector === "extract"}
          <div class="panel-scroll extraction-panel">
            <div class="panel-intro">
              <span class="eyebrow">STRUCTURED CONTENT</span>
              <h2>Put the page to work</h2>
              <p>{region ? "Working with selected " + region.kind + " region." : "Working with the current page. Select a region to isolate a table, formula, or form."}</p>
            </div>
            <div class="segmented">
              <button class:active={extractionKind === "fields"} onclick={() => { extractionKind = "fields"; editGroup = null; }}><Braces size={14} /> Fields</button>
              <button class:active={extractionKind === "table"} onclick={() => { extractionKind = "table"; editGroup = null; }}><Table2 size={14} /> Table</button>
              <button class:active={extractionKind === "formula"} onclick={() => { extractionKind = "formula"; editGroup = null; }}><Sigma size={14} /> Formula</button>
            </div>
            {#if extractionKind === "fields"}
              <label class="field-label">Fields to extract<textarea bind:value={labels} rows="2" placeholder="vendor, date, total, invoice number" disabled={locked}></textarea><small>Separate field names with commas. Extracted values remain editable.</small></label>
              <label class="setting-toggle"><span><strong>Use page layout</strong><small>LiLT uses word positions; otherwise GLiNER reads your corrected text.</small></span><input type="checkbox" bind:checked={useLayoutFields} disabled={locked || !capabilities.kie} /></label>
              <label class="field-label">Minimum field score<input type="number" min="0" max="1" step="0.05" bind:value={fieldThreshold} disabled={locked} /></label>
              <button class="button primary full" onclick={() => extract("fields")} disabled={locked || !activePage || !capabilities.kie || !labels.trim()}><Braces size={15} /> Extract fields</button>
              {#if !capabilities.kie}<p class="field-hint">Field extraction is unavailable in this build.</p>{/if}
            {:else if extractionKind === "table"}
              <div class="tool-explanation"><Table2 size={32} /><h3>Recover a table as editable cells</h3><p>Draw or select a table region for focused extraction. Each result keeps its own cells, header markers, and merged spans.</p></div>
              <button class="button primary full" onclick={() => extract("table")} disabled={locked || !activePage || !capabilities.table}><Table2 size={15} /> Extract table</button>
              {#if !capabilities.table}<p class="field-hint">Table recognition is unavailable in this build.</p>{/if}
            {:else}
              <div class="tool-explanation"><Sigma size={32} /><h3>Turn formulas into LaTeX</h3><p>Select a formula region, recognize it, and compare the editable notation with its captured source.</p></div>
              <button class="button primary full" onclick={() => extract("formula")} disabled={locked || !activePage || !capabilities.math}><Sigma size={15} /> Recognize formula</button>
              {#if !capabilities.math}<p class="field-hint">Formula recognition is unavailable in this build.</p>{/if}
            {/if}
            {#if activePage}<ExtractionObjectsPanel {project} page={activePage} kind={extractionKind}
              selectedId={extractionSelection[activePage.id + ":" + extractionKind]} disabled={locked} imageUrl={client.imageUrl}
              onselect={selectExtraction} onchange={changeExtraction} onendedit={() => (editGroup = null)}
              oncopy={copyExtraction} onsave={saveExtraction} onrepair={openTableRepair} onjoin={openTableJoin} />{/if}
          </div>
        {:else}
          <div class="panel-scroll export-panel">
            <div class="panel-intro">
              <span class="eyebrow">FINAL DOCUMENT</span>
              <h2>Ready for the next step</h2>
              <p>
                Your corrected text, reading order, and current page images are
                used directly in the export.
              </p>
            </div>
            <label class="field-label"
              >Output format<select bind:value={exportFormat} disabled={locked}
                >{#each capabilities.formats as format}<option value={format}
                    >{formatNames[format] ?? format}</option
                  >{/each}{#if !capabilities.formats.includes("pdfa")}<option value="pdfa" disabled>Archival PDF/A-2b — unavailable</option>{/if}</select
              ></label
            >
            <div class="format-card">
              <FileText size={23} />
              <div>
                <strong>{formatNames[exportFormat] ?? exportFormat}</strong>
                <p>
                  {exportFormat === "pdfa"
                    ? "Archival PDF/A-2b with embedded fonts, an sRGB color profile, and your searchable reviewed text."
                    : exportFormat === "pdf"
                    ? "Page images with a searchable text layer, using your reviewed text."
                    : exportFormat === "docx"
                      ? capabilities.structured_docx
                        ? "Editable Word structure: reviewed headings, styles, lists, reading columns and figures, with explicitly selected tables and formulas."
                        : "Editable paragraphs in page order, with text direction and blank lines preserved. Line endings become paragraph breaks; Plain text keeps the original line-ending characters. Complex page layout becomes flowing text."
                      : exportFormat === "xlsx"
                        ? "One worksheet for each explicitly selected saved table, with literal cell text, merged spans and your chosen tab order."
                      : exportFormat === "alto" || exportFormat === "hocr"
                        ? "Machine-readable text and page coordinates for downstream processing."
                        : exportFormat === "json"
                          ? "Structured page geometry, text regions, and confidence values."
                          : "Corrected text in the reading order shown in the workspace."}
                </p>
              </div>
            </div>
            {#if capabilities.pdf_export_reason}<p class="field-hint">
                {capabilities.pdf_export_reason}
              </p>{/if}
            {#if !capabilities.archival_pdf_export}<p class="field-hint">
                {capabilities.pdfa_export_reason ?? "PDF/A-2b export requires the matching archival renderer."}
              </p>{/if}
            {#if !capabilities.formats.includes("docx") && capabilities.docx_export_reason}<p class="field-hint">
                {capabilities.docx_export_reason}
              </p>{/if}
            {#if pdfExportSelected}
              <label class="field-label"
                >Page sizes
                <select bind:value={pdfScale} disabled={locked}>
                  <option value="source">Preserve source page sizes</option>
                  <option value="dpi">Use custom scan resolution</option>
                </select>
              </label>
              {#if pdfScale === "dpi"}
                <label class="field-label"
                  >Scan resolution (DPI)
                  <input
                    type="number"
                    min="36"
                    max="2400"
                    step="1"
                    bind:value={pdfDpi}
                    disabled={locked}
                  />
                </label>
              {/if}
              <p class="field-hint">
                Scan resolution determines printed page size. Page images
                without resolution metadata use 300 DPI by default.
              </p>
            {/if}
            <label class="field-label"
              >Pages<select bind:value={exportScope} disabled={locked}
                ><option value="all"
                  >All included pages ({metrics.included})</option
                ><option value="current">Current page</option><option
                  value="selected"
                  >Selected pages ({checkedPages.length})</option
                ></select
              ></label
            >
            <label class="setting-toggle"
              ><span
                ><strong>Require page approval</strong><small
                  >{exportFormat === "xlsx" ? "Block export until the source pages of all selected tables are approved." : "Block export until every included page is approved."}</small
                ></span
              ><input
                type="checkbox"
                bind:checked={requireReviewed}
                disabled={locked}
              /></label
            >
            {#if exportFormat === "docx" && capabilities.structured_docx}
              <OfficeExportPanel {project} pageIds={exportIds} value={officeOptions} disabled={locked}
                onchange={(next) => (officeOptions = next)} />
            {/if}
            {#if exportFormat === "xlsx" && capabilities.structured_xlsx}
              <WorkbookExportPanel {project} pageIds={exportIds} value={workbookOptions} disabled={locked}
                onchange={(next) => (workbookOptions = next)} />
            {/if}
            <div class="preflight-card" class:ready={!exportErrors.length}>
              <div class="section-heading">
                {#if exportErrors.length}<AlertTriangle size={17} /><strong
                    >{exportErrors.length} checks need attention</strong
                  >{:else}<CircleCheck size={17} /><strong
                    >{exportFormat === "xlsx" ? `Ready to export ${preflight.workbookOptions?.sheets.length ?? 0} ${preflight.workbookOptions?.sheets.length === 1 ? "worksheet" : "worksheets"} from ${preflight.pages.length} ${preflight.pages.length === 1 ? "source page" : "source pages"}` : `Ready to export ${preflight.pages.length} pages`}</strong
                  >{/if}
              </div>
              {#if exportErrors.length}<ul>
                  {#each exportErrors.slice(0, 8) as problem}<li>
                      {problem}
                    </li>{/each}
                </ul>
                {#if exportErrors.length > 8}<p>
                    And {exportErrors.length - 8} more checks need attention.
                  </p>{/if}{:else if exportFormat === "xlsx"}<div class="preflight-check">
                  <Check size={14} /> Every selected table is accounted for
                </div>
                <div class="preflight-check"><Check size={14} /> Literal cell values and spans included</div>
                <div class="preflight-check"><Check size={14} /> Worksheet names and limits checked</div>
                {#if requireReviewed}<div class="preflight-check"><Check size={14} /> Selected table source pages approved</div>{/if}
                {:else}<div class="preflight-check">
                  <Check size={14} /> Every selected page is accounted for
                </div>
                <div class="preflight-check">
                  <Check size={14} /> Page geometry matches the working image
                </div>
                <div class="preflight-check">
                  <Check size={14} /> Corrections and reading order included
                </div>
                {#if requireReviewed}<div class="preflight-check">
                    <Check size={14} /> All selected pages approved
                  </div>{/if}{/if}
              {#if exportTextCheck.affectedPages.length}<p class="field-hint">
                  Affected pages: {exportTextCheck.affectedPages.slice(0, 5).join(", ")}{exportTextCheck.affectedPages.length > 5 ? ` and ${exportTextCheck.affectedPages.length - 5} more` : ""}.
                </p>{/if}
              {#if exportTextCheck.alternatives.length}<div class="export-alternatives">
                  <p>Your page selection and reviewed text stay the same. Choose an available text format:</p>
                  <div class="button-grid">
                    {#each exportTextCheck.alternatives as format}<button class="button" disabled={locked}
                        onclick={() => { exportFormat = format; error = ""; }}>Use {formatNames[format] ?? format}</button>{/each}
                  </div>
                </div>{/if}
              {#each exportTextCheck.warnings as warning}<p class="inline-warning" role="status">{warning}</p>{/each}
              {#if preflight.warnings.length}<details>
                  <summary>{preflight.warnings.length} review notes</summary>
                  <ul>
                    {#each preflight.warnings as warning}<li>
                        {warning}
                      </li>{/each}
                  </ul>
                </details>{/if}
            </div>
            <button
              class="button primary full large"
              onclick={() => exportProject()}
              disabled={locked || !!exportErrors.length}
              ><Download size={17} /> Save {formatNames[exportFormat] ??
                exportFormat}</button
            >
            <p class="field-hint">
              Choose the output filename and location. Original documents and
              workspace images are protected from replacement.
            </p>
            {#if exportResultNotes?.warnings.length}
              <section class="export-result-notes" data-office-export-warnings aria-label="Export notes" aria-live="polite">
                <h3>{formatNames[exportResultNotes.format] ?? exportResultNotes.format} export completed with notes</h3>
                <p>{exportResultNotes.path}</p>
                <ul>{#each exportResultNotes.warnings as warning}<li>{warning}</li>{/each}</ul>
              </section>
            {/if}
            <div class="settings-section">
              <h3>Use the corrected text</h3>
              <div class="button-grid">
                <button
                  class="button"
                  onclick={() => copy(allText)}
                  disabled={!metrics.regions}
                  ><Copy size={14} /> Copy all text</button
                ><button
                  class="button"
                  onclick={reindex}
                  disabled={locked || !activePage}
                  ><Search size={14} /> Update search index</button
                >
              </div>
              <p class="field-hint">
                Update search reindexes the current source document after every
                included page of that document has been approved.
              </p>
            </div>
          </div>
        {/if}
      </aside>
    </div>
  {/if}
  <footer class="workspace-status" aria-live="polite">
    <span class="status-dot" class:working={!!busy || queueActive}></span><span
      class="status-message">{busy || notice || "Ready"}</span
    >
    {#if canUndoPreparation && !busy}<button class="button undo-preparation" onclick={undoPreparation} disabled={locked}>Undo preparation</button>{/if}
    {#if importController}<button class="button cancel-import" onclick={cancelImport} disabled={importStopping}>{importStopping ? "Stopping…" : "Cancel import"}</button>{/if}
    {#if exportController && exportPhase}<button class="button cancel-export" onclick={cancelExport} disabled={!exportCancellable || exportStopping}
        title={exportPhase === "destination" ? "Use the system save dialog to finish or cancel" : exportPhase === "committing" ? "The export is finishing" : "Stop the export before it is saved"}
      >{exportStopping ? "Stopping…" : "Cancel export"}</button>{/if}
    <div class="toolbar-spacer"></div>
  {#if queueActive || ((queueState.phase === "completed" || queueState.phase === "cancelled") && queueBarDismissedFor !== queueState)}
      <div class="queue-bar">
        <div class="queue-label">
          {#if queueActive}<LoaderCircle
              size={15}
              class={queueState.phase !== "paused" ? "spin" : ""}
            />{:else}<CircleCheck size={15} />{/if}<strong
            >{queueState.phase === "paused"
              ? "Queue paused"
              : queueState.phase === "pausing"
                ? "Pausing after this page"
                : queueState.phase === "cancelling"
                  ? "Stopping after this page"
                  : queueState.phase === "cancelled"
                    ? "Queue stopped"
                    : queueState.phase === "completed"
                      ? "Batch complete"
                      : "Recognizing documents"}</strong
          ><span
            >{queueState.completed + queueState.failed} / {queueState.total} pages{queueState.failed
              ? ` · ${queueState.failed} failed`
              : ""}</span
          >
        </div>
        <progress
          max={queueState.total || 1}
          value={queueState.completed + queueState.failed}
          aria-label="Recognition progress"
        ></progress>
        {#if queueActive}<div class="queue-actions">
            {#if queueState.phase === "paused"}<button
                class="button small"
                onclick={resumeQueue}
                disabled={!!busy || queuePromise !== null}><Play size={13} /> Resume</button
              >{:else}<button
                class="button small"
                onclick={() => queue.pause()}
                disabled={queueState.phase !== "running"}
                ><Pause size={13} /> Pause</button
              >{/if}<button
              class="button small"
              onclick={stopQueue}
              disabled={queueState.phase === "cancelling" || (queueState.phase === "paused" && queuePromise !== null)}
              ><Square size={12} /> Stop</button
            >
          </div>{:else if queueState.failed}<button
            class="button small"
            onclick={() => recognize("failed")}
            disabled={locked}><RefreshCw size={13} /> Retry failed</button
          >{/if}
      </div>
    {/if}
    {#if project.pages.length && !importController && !exportController}<span
        >{metrics.recognized}/{metrics.included} recognized</span
      ><span class="status-divider"></span><span class="approved-stat"
        ><CheckCheck size={12} />{metrics.approved} approved</span
      ><span class="status-divider"></span><span
        class:attention-stat={metrics.needsReview > 0}
        >{metrics.needsReview} to review</span
      >{/if}
  </footer>
</section>

{#if tableJoin}
  <TableJoinDialog {project} projectId={tableJoin.projectId} initialPageId={tableJoin.pageId} initialObject={tableJoin.object}
    disabled={locked} imageUrl={client.imageUrl} onapply={applyTableJoin} onclose={() => (tableJoin = null)} />
{/if}

{#if tableRepair}
  <TableRepairDialog {project} projectId={tableRepair.projectId} pageId={tableRepair.pageId}
    initialObject={tableRepair.object} initialCellId={tableRepair.cellId}
    disabled={locked} imageUrl={client.imageUrl} onapply={applyTableRepair} onclose={() => (tableRepair = null)} />
{/if}

{#if pageTextDraftOpen && project.pageTextDraft}
  {#key project.pageTextDraft.id}
    <PageTextDraftDialog
      draft={project.pageTextDraft}
      {saveStatus}
      disabled={locked}
      imageUrl={client.imageUrl}
      onupdate={updateOpenPageTextDraft}
      onreview={() => preparePageTextEdit(project)}
      onapply={applyOpenPageTextDraft}
      ondiscard={discardOpenPageTextDraft}
      onclose={() => { pageTextDraftOpen = false; pageTextDraftLocalUnsaved = false; }}
      onsave={async () => { await autosave?.flush(); }}
      onlocalunsaved={(unsaved) => (pageTextDraftLocalUnsaved = unsaved)}
    />
  {/key}
{/if}

{#if passageAction}
  <PassageToolsDialog
    project={passageAction.project}
    pageId={passageAction.pageId}
    regionId={passageAction.regionId}
    mode={passageAction.mode}
    offset={passageAction.offset}
    imageUrl={client.imageUrl}
    onapply={applyStructuralPassage}
    onrepair={applyTextRepair}
    onclose={() => (passageAction = null)}
  />
{/if}

{#if confirm}<dialog
    class="workspace-dialog"
    class:ocr-preview-dialog={!!confirm.ocrPreview || !!confirm.correctionPreview}
    bind:this={actionDialog}
    onclose={() => {
      if (confirm?.ocrPreview) notice = "Current text kept. The partial OCR result was not applied.";
      confirm = null;
    }}
    aria-labelledby="ocr-confirm-title"
  >
    <h2 id="ocr-confirm-title">{confirm.title}</h2>
    <p>{confirm.message}</p>
    {#if confirm.correctionPreview}
      <CorrectionPreviewPanel
        preview={confirm.correctionPreview}
        selectedIds={correctionSelectedIds}
        onselect={(ids) => (correctionSelectedIds = ids)}
      />
    {/if}
    {#if confirm.ocrPreview}
      <div class="ocr-preview-comparison">
        <label>
          Current text
          <textarea
            readonly
            aria-label="Current text"
            value={confirm.ocrPreview.request.previousText}
            placeholder="No existing text in this area"
            dir="auto"
          ></textarea>
        </label>
        <label>
          New OCR text
          <textarea
            readonly
            aria-label="New OCR text"
            value={confirm.ocrPreview.replacementText}
            dir="auto"
          ></textarea>
        </label>
      </div>
      {#if confirm.ocrPreview.warnings.length}
        <ul class="ocr-preview-warnings">
          {#each confirm.ocrPreview.warnings as warning}<li>{warning}</li>{/each}
        </ul>
      {/if}
      <p class="ocr-preview-note">Using the new result replaces text only in this area and clears its approval. Other passages keep their corrections. Undo restores the previous result.</p>
    {/if}
    {#if confirm.splitWidth}
      <label class="field-label"
        >Split position from left edge (pixels)
        <input
          type="number"
          min="1"
          max={confirm.splitWidth - 1}
          step="1"
          bind:value={splitPosition}
          aria-label="Split position in pixels"
        />
        <small
          >Page width: {confirm.splitWidth} pixels. The default divides it in half.</small
        >
      </label>
    {/if}
    <div class="dialog-actions">
      <button class="button" onclick={() => {
        if (confirm?.ocrPreview) notice = "Current text kept. The partial OCR result was not applied.";
        confirm = null;
      }}>{confirm.ocrPreview ? "Keep current text" : "Cancel"}</button
      >{#if !confirm.correctionPreview || confirm.correctionPreview.matches.length}<button
        class="button primary"
        disabled={(!!confirm.correctionPreview && !correctionSelectedIds.length) ||
          (!!confirm.splitWidth &&
            (!Number.isFinite(splitPosition) ||
              splitPosition < 1 ||
              splitPosition >= confirm.splitWidth))}
        onclick={() => {
          const action = confirm?.action;
          confirm = null;
          void action?.();
        }}>{confirm.correctionPreview
          ? `Apply ${correctionSelectedIds.length} correction${correctionSelectedIds.length === 1 ? "" : "s"}`
          : confirm.label}</button
      >{/if}
    </div>
  </dialog>{/if}
{#if helpOpen}<dialog
    class="workspace-dialog"
    bind:this={helpDialog}
    onclose={() => (helpOpen = false)}
    aria-labelledby="ocr-help-title"
  >
    <div class="section-heading">
      <h2 id="ocr-help-title">Keyboard shortcuts</h2>
      <button
        class="icon-button"
        onclick={() => (helpOpen = false)}
        aria-label="Close shortcuts"><X size={18} /></button
      >
    </div>
    <dl class="shortcuts">
      <dt>Save project</dt>
      <dd>Ctrl / ⌘ S</dd>
      <dt>Find and replace</dt>
      <dd>Ctrl / ⌘ F</dd>
      <dt>Undo workspace change</dt>
      <dd>Ctrl / ⌘ Z</dd>
      <dt>Redo workspace change</dt>
      <dd>Ctrl / ⌘ Shift Z</dd>
      <dt>Next review issue</dt>
      <dd>F8</dd>
      <dt>Previous / next page</dt>
      <dd>Alt ← / →</dd>
      <dt>Exit drawing tool</dt>
      <dd>Esc</dd>
    </dl>
    <p class="field-hint">
      Inside text fields, native text editing shortcuts apply. Workspace Undo
      remains available in the toolbar.
    </p>
  </dialog>{/if}

<style>
  .ocr-workspace {
    --surface: #18181b;
    --panel: #18181b;
    --line: #27272a;
    --muted: #a1a1aa;
    --text: #e4e4e7;
    --blue: #60a5fa;
    --accent: #3b82f6;
    --green: #4ade80;
    --amber: #eab308;
    display: flex;
    flex-direction: column;
    height: 100%;
    min-height: 0;
    position: relative;
    color: var(--text);
    background: var(--surface);
    font-size: 12px;
    font-family:
      Inter,
      -apple-system,
      BlinkMacSystemFont,
      "Segoe UI",
      sans-serif;
    overflow: hidden;
  }
  .page-window-controls {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: 4px 8px;
    border-top: 1px solid var(--line);
    color: var(--muted);
    font-size: 10px;
  }
  .recognition-notes {
    padding: 8px 14px;
    border-bottom: 1px solid #52525b;
    background: #27272a;
    color: #fcd34d;
    font-size: 11px;
  }
  .recognition-notes summary {
    display: flex;
    align-items: center;
    gap: 7px;
    cursor: pointer;
  }
  .recognition-notes ul {
    max-height: 110px;
    overflow: auto;
    padding-left: 18px;
    line-height: 1.5;
  }
  .ocr-workspace :global(button),
  .ocr-workspace :global(input),
  .ocr-workspace :global(select),
  .ocr-workspace :global(textarea) {
    font: inherit;
    box-sizing: border-box;
  }
  .ocr-workspace :global(button) {
    cursor: pointer;
  }
  .ocr-workspace :global(button:disabled) {
    cursor: default;
    opacity: 0.4;
  }
  .ocr-workspace :global(button:focus-visible),
  .ocr-workspace :global(input:focus-visible),
  .ocr-workspace :global(textarea:focus-visible),
  .ocr-workspace :global(select:focus-visible) {
    outline: 2px solid #60a5fa;
    outline-offset: 2px;
  }
  .ocr-workspace :global(input:not([type="checkbox"]):not([type="range"])),
  .ocr-workspace :global(select),
  .ocr-workspace :global(textarea) {
    background: #18181b;
    color: var(--text);
    border: 1px solid #3f3f46;
    border-radius: 6px;
    padding: 8px 10px;
    min-width: 0;
    line-height: 1.45;
  }
  .ocr-workspace :global(input::placeholder),
  .ocr-workspace :global(textarea::placeholder) {
    color: #71717a;
  }
  .ocr-workspace :global(input[type="checkbox"]) {
    accent-color: #3b82f6;
    width: 14px;
    height: 14px;
    margin: 0;
    flex: none;
  }
  .ocr-workspace :global(input[type="range"]) {
    accent-color: #3b82f6;
    width: 100%;
  }
  .workspace-header {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 17px 22px 15px;
    min-height: 75px;
    border-bottom: 1px solid var(--line);
    background: #18181b;
    flex: none;
  }
  .workspace-mark {
    height: 41px;
    width: 41px;
    display: grid;
    place-items: center;
    background: #3f3f46;
    border: 1px solid #2563eb;
    border-radius: 11px;
    color: #93c5fd;
    flex: none;
  }
  .workspace-title {
    min-width: 100px;
    flex: 1;
  }
  .eyebrow {
    font-size: 9px;
    font-weight: 700;
    letter-spacing: 1.5px;
    color: #a1a1aa;
    display: flex;
    align-items: center;
    gap: 9px;
  }
  .local-badge {
    font-size: 8px;
    letter-spacing: 0.7px;
    padding: 2px 5px;
    border: 1px solid #52525b;
    border-radius: 3px;
    color: #4ade80;
    background: #052e16;
  }
  .workspace-title input {
    font-size: 18px !important;
    letter-spacing: -0.35px;
    font-weight: 600 !important;
    border: 0 !important;
    background: transparent !important;
    padding: 3px 0 0 !important;
    width: min(100%, 510px);
    text-overflow: ellipsis;
  }
  .header-actions {
    display: flex;
    align-items: center;
    gap: 7px;
    flex: none;
  }
  .save-state {
    display: flex;
    align-items: center;
    gap: 6px;
    color: #a1a1aa;
    font-size: 10px;
    white-space: nowrap;
    margin: 0 8px;
  }
  .save-state.save-error {
    color: #fca5a5;
  }
  .save-state button {
    background: transparent;
    color: #93c5fd;
    border: 0;
    text-decoration: underline;
  }
  .button {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    gap: 7px;
    border: 1px solid #3f3f46;
    border-radius: 6px;
    background: #27272a;
    color: #dbeafe;
    padding: 7px 11px;
    white-space: nowrap;
    font-size: 11px;
    font-weight: 550;
    min-height: 32px;
    transition:
      background 0.12s,
      border-color 0.12s;
  }
  .button:hover:not(:disabled) {
    background: #3f3f46;
    border-color: #71717a;
  }
  .button.primary {
    background: #3b82f6;
    border-color: #3b82f6;
    color: #fff;
    box-shadow: 0 1px 2px #0003;
  }
  .button.primary:hover:not(:disabled) {
    background: #3b82f6;
  }
  .button.subtle {
    background: transparent;
    border-color: transparent;
    color: #d4d4d8;
  }
  .button.subtle:hover:not(:disabled),
  .button.subtle.active {
    background: #3f3f46;
    border-color: #52525b;
    color: #bfdbfe;
  }
  .button.small {
    min-height: 27px;
    padding: 4px 8px;
    font-size: 10px;
  }
  .button.large {
    padding: 11px 17px;
    min-height: 42px;
    font-size: 12px;
  }
  .button.full {
    width: 100%;
  }
  .button.export-button {
    background: #3f3f46;
    border-color: #2563eb;
    color: #bfdbfe;
  }
  .button.approved-button {
    background: #14532d;
    border-color: #15803d;
    color: #86efac;
  }
  .icon-button {
    border: 1px solid transparent;
    background: transparent;
    color: #a1a1aa;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    border-radius: 5px;
    padding: 6px;
    min-width: 28px;
    min-height: 28px;
    flex: none;
  }
  .icon-button:hover:not(:disabled),
  .icon-button.active {
    background: #3f3f46;
    color: #dbeafe;
    border-color: #52525b;
  }
  .text-button {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    background: transparent;
    border: 0;
    color: #93c5fd;
    padding: 3px 0;
    font-size: 10px;
    white-space: nowrap;
  }
  .danger {
    color: #fca5a5 !important;
  }
  .toolbar-spacer {
    flex: 1;
    min-width: 4px;
  }
  .command-bar {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 11px 20px;
    border-bottom: 1px solid var(--line);
    background: #18181b;
    flex: none;
    min-width: 0;
  }
  .command-separator {
    height: 22px;
    width: 1px;
    background: var(--line);
    margin: 0 4px;
  }
  .undo-group,
  .tool-group {
    display: flex;
    align-items: center;
    gap: 3px;
  }
  .undo-group {
    border-right: 1px solid var(--line);
    padding-right: 9px;
    margin-right: 2px;
  }
  .feedback {
    display: flex;
    align-items: flex-start;
    gap: 10px;
    padding: 10px 18px;
    flex: none;
    font-size: 11px;
  }
  .page-text-draft-banner { display: flex; flex-wrap: wrap; flex: none; align-items: center; gap: 10px; min-width: 0; padding: 10px 18px; border-bottom: 1px solid #52525b; background: #27272a; color: #bfdbfe; }
  .page-text-draft-banner > div { display: flex; flex: 1; flex-direction: column; gap: 3px; min-width: 150px; }
  .page-text-draft-banner strong { font-size: 12px; }
  .page-text-draft-banner span { color: #93c5fd; font-size: 11px; overflow-wrap: anywhere; }
  .feedback > span {
    flex: 1;
    white-space: pre-wrap;
    word-break: break-word;
    max-height: 110px;
    overflow: auto;
  }
  .feedback.error {
    background: #27272a;
    color: #fca5a5;
    border-bottom: 1px solid #3f3f46;
  }
  .feedback :global(svg) {
    flex: none;
  }
  .queue-bar {
    display: flex;
    align-items: center;
    gap: 16px;
    padding: 9px 20px;
    background: #27272a;
    border-bottom: 1px solid #1d4ed8;
    font-size: 11px;
    flex: none;
  }
  .queue-label {
    display: flex;
    align-items: center;
    gap: 7px;
    flex: none;
  }
  .queue-label > span {
    color: #a1a1aa;
  }
  .queue-bar progress {
    height: 5px;
    width: 100%;
    max-width: 300px;
    accent-color: #60a5fa;
    border: 0;
    border-radius: 5px;
    overflow: hidden;
  }
  .queue-bar progress::-webkit-progress-bar {
    background: #3f3f46;
  }
  .queue-bar progress::-webkit-progress-value {
    background: #60a5fa;
  }
  .queue-actions {
    display: flex;
    gap: 6px;
    margin-left: auto;
  }
  .work-area {
    display: grid;
    grid-template-columns: 218px minmax(280px, 1fr) 390px;
    flex: 1;
    min-height: 0;
    min-width: 0;
    overflow: hidden;
  }
  .work-area.rail-hidden {
    grid-template-columns: minmax(280px, 1fr) 390px;
  }
  .page-rail {
    display: flex;
    flex-direction: column;
    min-height: 0;
    background: #18181b;
    border-right: 1px solid var(--line);
  }
  .rail-heading {
    height: 46px;
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 0 13px 0 17px;
    flex: none;
  }
  .rail-heading strong {
    font-size: 12px;
  }
  .rail-heading strong span {
    font-size: 10px;
    background: #3f3f46;
    padding: 2px 6px;
    border-radius: 4px;
    color: #d4d4d8;
    margin-left: 5px;
    font-weight: 500;
  }
  .rail-controls {
    padding: 0 12px 9px;
    display: flex;
    flex-direction: column;
    gap: 7px;
  }
  .rail-controls select {
    font-size: 10px;
    padding: 6px 8px !important;
  }
  .search-input {
    position: relative;
    display: flex;
    align-items: center;
  }
  .search-input :global(svg) {
    position: absolute;
    left: 8px;
    color: #71717a;
  }
  .search-input input {
    width: 100%;
    padding-left: 27px !important;
    font-size: 10px !important;
    background: #18181b !important;
    border-color: #3f3f46 !important;
  }
  .selection-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 7px 14px;
    border-top: 1px solid var(--line);
    border-bottom: 1px solid var(--line);
    font-size: 10px;
    color: var(--muted);
  }
  .selection-row label {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .page-list {
    flex: 1;
    overflow: auto;
    padding: 8px 9px;
    min-height: 0;
  }
  .page-card {
    display: flex;
    gap: 7px;
    position: relative;
    border: 1px solid transparent;
    border-radius: 7px;
    padding: 9px 6px;
    margin-bottom: 5px;
    min-height: 100px;
  }
  .page-card:hover {
    background: #27272a;
  }
  .page-card.current {
    background: #27272a;
    border-color: #2563eb;
    box-shadow: inset 3px 0 #60a5fa;
  }
  .page-card.excluded {
    opacity: 0.48;
  }
  .page-checkbox {
    margin-top: 28px !important;
  }
  .page-card-main {
    border: 0;
    padding: 0;
    background: none;
    display: flex;
    gap: 9px;
    flex: 1;
    min-width: 0;
    text-align: left;
    color: inherit;
  }
  .thumbnail {
    height: 72px;
    width: 50px;
    flex: none;
    display: flex;
    align-items: center;
    justify-content: center;
    position: relative;
    filter: drop-shadow(0 2px 4px #0005);
  }
  .thumbnail img {
    max-width: 100%;
    max-height: 100%;
    object-fit: contain;
    border: 1px solid #a1a1aa;
    background: white;
  }
  .thumb-approved {
    position: absolute;
    right: -4px;
    bottom: -3px;
    background: #16a34a;
    border: 2px solid #052e16;
    border-radius: 50%;
    width: 18px;
    height: 18px;
    display: grid;
    place-items: center;
    color: white;
  }
  .page-card-info {
    display: flex;
    flex-direction: column;
    min-width: 0;
    flex: 1;
    gap: 5px;
    padding-top: 3px;
  }
  .page-card-info > strong {
    font-size: 11px;
    font-weight: 650;
    color: #dbeafe;
  }
  .page-filename {
    font-size: 9px;
    line-height: 1.4;
    color: #a1a1aa;
    overflow: hidden;
    text-overflow: ellipsis;
    display: -webkit-box;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    -webkit-box-orient: vertical;
    word-break: break-word;
  }
  .page-state {
    font-size: 9px;
    display: flex;
    align-items: center;
    gap: 4px;
    color: #a1a1aa;
    margin-top: auto;
  }
  .page-state.approved {
    color: #4ade80;
  }
  .page-state.failed {
    color: #fca5a5;
  }
  .amber-dot {
    display: inline-block;
    background: #eab308;
    width: 5px;
    height: 5px;
    border-radius: 50%;
  }
  .rail-empty {
    color: var(--muted);
    padding: 20px 6px;
    text-align: center;
    font-size: 11px;
  }
  .range-control {
    padding: 9px 12px;
    border-top: 1px solid var(--line);
    flex: none;
  }
  .range-control > label {
    color: #a1a1aa;
    font-size: 9px;
    display: block;
    margin-bottom: 5px;
  }
  .range-control > div {
    display: flex;
    gap: 4px;
  }
  .range-control input {
    width: 100%;
    padding: 5px 8px !important;
    font-size: 10px !important;
  }
  .rail-footer {
    display: flex;
    gap: 3px;
    padding: 7px 10px;
    border-top: 1px solid var(--line);
    flex: none;
  }
  .document-pane {
    min-width: 0;
    min-height: 0;
    display: flex;
    flex-direction: column;
    overflow: hidden;
  }
  .document-pane :global(.canvas-shell) {
    flex: 1;
    min-height: 0;
  }
  .image-toolbar {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px 9px;
    padding: 6px 10px;
    min-height: 47px;
    background: #18181b;
    border-bottom: 1px solid var(--line);
    flex: none;
  }
  .image-toolbar select {
    font-size: 10px !important;
    padding: 4px 18px 4px 5px !important;
    background: transparent !important;
    border-color: transparent !important;
    max-width: 132px;
  }
  .image-toolbar .tool-group + .tool-group {
    border-left: 1px solid var(--line);
    padding-left: 7px;
  }
  .image-footer {
    display: flex;
    align-items: center;
    gap: 5px;
    flex: none;
    height: 43px;
    padding: 0 12px;
    background: #18181b;
    border-top: 1px solid var(--line);
  }
  .image-footer select {
    font-size: 10px !important;
    padding: 4px 6px !important;
    background: #27272a !important;
    border-color: #3f3f46 !important;
    min-width: 88px;
  }
  .page-counter {
    font-size: 11px;
    font-variant-numeric: tabular-nums;
    font-weight: 600;
    padding: 0 4px;
  }
  .page-counter > span {
    color: #a1a1aa;
    font-weight: 400;
  }
  .inspector {
    border-left: 1px solid var(--line);
    display: flex;
    flex-direction: column;
    min-height: 0;
    overflow: hidden;
    background: #18181b;
  }
  .inspector-tabs {
    display: flex;
    border-bottom: 1px solid #3f3f46;
    padding: 0 8px;
    background: #18181b;
    height: 47px;
    flex: none;
    gap: 2px;
  }
  .inspector-tabs > button {
    display: flex;
    align-items: center;
    gap: 5px;
    flex: 1;
    justify-content: center;
    background: none;
    border: 0;
    border-bottom: 2px solid transparent;
    color: #a1a1aa;
    font-size: 10px;
    padding: 0 5px;
  }
  .inspector-tabs > button.active {
    color: #93c5fd;
    border-bottom-color: #60a5fa;
    background: linear-gradient(transparent, #243b563f);
  }
  .review-heading {
    padding: 15px 16px 12px;
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    align-items: center;
    justify-content: space-between;
  }
  .review-heading > div { flex: 1; min-width: 120px; }
  .review-heading h2 {
    font-size: 14px;
    font-weight: 650;
    margin: 0 0 4px;
    letter-spacing: -0.2px;
  }
  .review-heading span {
    font-size: 10px;
    color: #a1a1aa;
  }
  .review-options {
    display: flex;
    gap: 6px;
    align-items: center;
    padding: 9px 16px;
    border-top: 1px solid #27272a;
    font-size: 10px;
  }
  .check-label {
    display: flex;
    align-items: center;
    gap: 7px;
    font-size: 10px;
    color: #d4d4d8;
    line-height: 1.5;
  }
  .review-view-tabs {
    display: flex;
    gap: 12px;
    align-items: center;
    padding: 0 16px;
    border-top: 1px solid #27272a;
    border-bottom: 1px solid #27272a;
    height: 34px;
  }
  .review-view-tabs button {
    border: 0;
    background: none;
    color: #a1a1aa;
    font-size: 10px;
    padding: 8px 0;
  }
  .review-view-tabs button.active {
    color: #bfdbfe;
    border-bottom: 1px solid #60a5fa;
  }
  .review-view-tabs > span {
    margin-left: auto;
    font-size: 9px;
    color: #71717a;
  }
  .find-panel {
    background: #18181b;
    padding: 10px 12px;
    border-top: 1px solid #3f3f46;
    border-bottom: 1px solid #3f3f46;
    display: flex;
    flex-direction: column;
    gap: 7px;
  }
  .find-row {
    display: flex;
    gap: 4px;
    align-items: center;
  }
  .find-row input {
    width: 100%;
    padding: 6px 8px !important;
    font-size: 11px !important;
  }
  .find-row > span {
    font-size: 10px;
    color: #a1a1aa;
  }
  .find-panel .check-label {
    font-size: 9px;
  }
  .find-panel .muted {
    font-size: 9px;
  }
  .source-loupe {
    padding: 10px 14px 12px;
    background: #18181b;
    border-bottom: 1px solid #3f3f46;
    flex: none;
  }
  .loupe-label {
    font-size: 8px;
    color: #a1a1aa;
    letter-spacing: 1.1px;
    display: flex;
    align-items: center;
    gap: 5px;
    margin-bottom: 7px;
  }
  .loupe-label > span {
    margin-left: auto;
    min-width: 0;
    max-width: 75%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    letter-spacing: 0;
    color: #a1a1aa;
  }
  .source-loupe > svg {
    background: #fde68a;
    width: 100%;
    height: 60px;
    display: block;
    border-radius: 4px;
    box-shadow: 0 1px 4px #0003;
  }
  .region-list {
    flex: 1;
    min-height: 80px;
    overflow: auto;
    padding: 12px;
  }
  .region-card {
    border: 1px solid #3f3f46;
    border-left: 3px solid #3f3f46;
    border-radius: 6px;
    background: #27272a;
    margin-bottom: 10px;
    overflow: hidden;
  }
  .region-card.attention {
    border-left-color: #eab308;
  }
  .region-card.selected {
    border-color: #3b82f6;
    border-left-color: #60a5fa;
    box-shadow: 0 0 0 1px #32629433;
    background: #27272a;
  }
  .region-card.excluded {
    opacity: 0.45;
  }
  .region-card-heading {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 8px 9px 6px;
  }
  .region-number {
    border: 1px solid #52525b;
    border-radius: 4px;
    background: #27272a;
    color: #bfdbfe;
    min-width: 21px;
    height: 20px;
    font-size: 10px;
  }
  .region-kind {
    background: none;
    border: 0;
    color: #a1a1aa;
    font-size: 9px;
    text-transform: uppercase;
    letter-spacing: 0.7px;
    padding: 0 2px;
  }
  .confidence {
    margin-left: auto;
    font-size: 9px;
    color: #fbbf24;
    font-variant-numeric: tabular-nums;
    border: 1px solid #80642e44;
    background: #80642e22;
    border-radius: 4px;
    padding: 2px 5px;
  }
  .confidence.unknown {
    color: #a1a1aa;
    border-color: #53627b44;
    background: #53627b22;
  }
  .region-approved {
    color: #4ade80;
    display: flex;
  }
  .edited-badge {
    color: #93c5fd;
    font-size: 9px;
  }
  .shared-source-badge { color: #fbbf24; font-size: 9px; }
  .region-card textarea {
    border: 0 !important;
    border-top: 1px solid #2e3c5055 !important;
    resize: vertical;
    width: 100%;
    display: block;
    border-radius: 0 !important;
    min-height: 56px;
    background: transparent !important;
    font-size: 12px !important;
    line-height: 1.65 !important;
    padding: 8px 10px !important;
    color: #dbeafe !important;
  }
  .region-card textarea:focus {
    background: #18181b !important;
  }
  .character-evidence {
    padding: 3px 10px 6px;
    white-space: pre-wrap;
    font-size: 10px;
    line-height: 1.7;
    color: #a1a1aa;
    overflow-wrap: anywhere;
  }
  .evidence-label {
    font-size: 8px;
    letter-spacing: 0.5px;
    margin-right: 7px;
    color: #71717a;
  }
  .character-evidence .uncertain {
    background: #a9672d55;
    color: #fcd34d;
    border-bottom: 1px solid #d97706;
  }
  .region-actions {
    padding: 6px 9px;
    border-top: 1px solid #2d3c5144;
    display: flex;
    gap: 6px;
    align-items: center;
    min-width: 0;
  }
  .region-edit-actions {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 7px 14px;
    padding: 7px 10px;
    border-top: 1px solid #3f3f46;
  }
  .region-edit-actions .text-button {
    font-size: 11px;
  }
  .region-actions .icon-button {
    min-width: 23px;
    min-height: 23px;
    padding: 4px;
  }
  .region-actions .text-button {
    font-size: 9px;
  }
  .region-properties {
    border-top: 1px solid #52525b;
    padding: 10px 13px;
    background: #18181b;
    max-height: 250px;
    overflow: auto;
    flex: none;
  }
  .region-properties .field-grid {
    grid-template-columns: repeat(4, 1fr);
    gap: 6px;
  }
  .region-properties .field-grid > label:first-child {
    grid-column: 1/-1;
  }
  .region-properties label {
    display: flex;
    flex-direction: column;
    gap: 4px;
    font-size: 9px;
    color: #a1a1aa;
  }
  .region-properties label.check-label {
    flex-direction: row;
  }
  .region-properties input,
  .region-properties select {
    width: 100%;
    padding: 5px 6px !important;
    font-size: 10px !important;
  }
  .region-properties .check-label input {
    width: 13px;
  }
  .original-text {
    font-size: 11px;
    white-space: pre-wrap;
    line-height: 1.5;
    color: #a1a1aa;
  }
  .region-properties details {
    font-size: 10px;
    margin-top: 9px;
    color: #a1a1aa;
  }
  .approval-bar {
    display: flex;
    align-items: center;
    gap: 7px;
    padding: 11px 12px;
    border-top: 1px solid #3f3f46;
    background: #27272a;
    flex: none;
  }
  .approval-bar > span {
    color: #4ade80;
  }
  .approval-bar .button {
    font-size: 10px;
    flex: 1;
    padding: 8px;
  }
  .reading-order {
    display: flex;
    flex-direction: column;
    gap: 10px;
    min-height: 100%;
  }
  .panel-message {
    display: flex;
    flex-direction: column;
    align-items: center;
    text-align: center;
    gap: 10px;
    padding: 40px 22px;
    color: #a1a1aa;
  }
  .panel-message :global(svg) {
    color: #3b82f6;
  }
  .panel-message h3 {
    font-size: 13px;
    color: #d4d4d8;
    margin: 4px 0 0;
  }
  .panel-message p {
    font-size: 11px;
    line-height: 1.75;
    margin: 0 0 5px;
    color: #a1a1aa;
    overflow-wrap: anywhere;
  }
  .panel-message.warning :global(svg) {
    color: #eab308;
  }
  .panel-scroll {
    overflow: auto;
    padding: 18px;
    flex: 1;
    min-height: 0;
  }
  .panel-intro {
    margin-bottom: 20px;
  }
  .panel-intro h2 {
    font-size: 18px;
    line-height: 1.35;
    letter-spacing: -0.4px;
    margin: 9px 0 8px;
    color: #dbeafe;
  }
  .panel-intro p {
    font-size: 11px;
    line-height: 1.7;
    color: #a1a1aa;
    margin: 0;
  }
  .field-label {
    display: flex;
    flex-direction: column;
    gap: 7px;
    color: #d4d4d8;
    font-size: 10px;
    font-weight: 600;
    margin-bottom: 14px;
  }
  .field-label input,
  .field-label textarea,
  .field-label select {
    width: 100%;
    font-weight: 400 !important;
  }
  .field-label small {
    color: #a1a1aa;
    font-size: 10px;
    font-weight: 400;
    line-height: 1.6;
  }
  .field-hint {
    font-size: 10px;
    line-height: 1.7;
    color: #a1a1aa;
    margin: 9px 0 14px;
  }
  .field-grid {
    display: grid;
    gap: 10px;
  }
  .field-grid.two {
    grid-template-columns: 1fr 1fr;
  }
  .field-grid.two .field-label {
    min-width: 0;
    margin-bottom: 4px;
  }
  .field-grid.two select {
    font-size: 10px !important;
    padding: 7px 5px !important;
  }
  .settings-section {
    padding-top: 16px;
    margin-top: 18px;
    border-top: 1px solid #3f3f46;
  }
  .settings-section h3 {
    display: flex;
    gap: 7px;
    align-items: center;
    font-size: 11px;
    color: #bfdbfe;
    margin: 0 0 12px;
    font-weight: 600;
  }
  .setting-toggle {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 18px;
    padding: 9px 0;
  }
  .setting-toggle > span {
    display: flex;
    flex-direction: column;
    gap: 3px;
  }
  .setting-toggle strong {
    font-size: 11px;
    font-weight: 500;
    color: #d4d4d8;
  }
  .setting-toggle small {
    font-size: 10px;
    color: #a1a1aa;
    line-height: 1.5;
  }
  .settings-section > .button {
    margin-top: 12px;
  }
  .threshold-label {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    align-items: center;
    font-size: 10px;
    color: #a1a1aa;
  }
  .threshold-label strong {
    margin-left: auto;
    color: #bfdbfe;
  }
  .threshold-label input {
    flex: 0 0 100%;
    margin-top: 8px;
  }
  .advanced-settings {
    border: 1px solid #3f3f46;
    border-radius: 6px;
    margin-top: 18px;
    padding: 10px;
    color: #60a5fa;
    font-size: 10px;
  }
  .advanced-settings summary {
    cursor: pointer;
    font-weight: 500;
    list-style-position: inside;
  }
  .advanced-settings[open] summary {
    margin-bottom: 12px;
  }
  .advanced-settings > .button {
    margin-top: 6px;
  }
  .config-editor,
  .provenance {
    font-family: ui-monospace, SFMono-Regular, Consolas, monospace !important;
    font-size: 10px !important;
  }
  .provenance {
    white-space: pre-wrap;
    word-break: break-all;
    color: #a1a1aa;
    line-height: 1.7;
    max-height: 350px;
    overflow: auto;
  }
  .text-error {
    color: #fca5a5;
    font-size: 10px;
  }
  .model-install {
    margin: 14px 0;
    padding: 14px;
    border: 1px solid var(--ocr-border, #3f3f46);
    border-radius: 8px;
  }
  .model-install h3 { margin: 0 0 8px; font-size: 13px; }
  .model-install .button { min-height: 44px; white-space: normal; width: 100%; justify-content: center; }
  .model-progress { font-size: 12px; line-height: 1.6; overflow-wrap: anywhere; }
  .inline-warning {
    font-size: 10px;
    line-height: 1.7;
    padding: 10px;
    background: #65461c22;
    border: 1px solid #8a642b55;
    color: #fbbf24;
    border-radius: 6px;
    display: flex;
    align-items: flex-start;
    gap: 8px;
    margin-bottom: 14px;
  }
  .inline-warning :global(svg) {
    flex: none;
  }
  .button-grid {
    display: flex;
    gap: 7px;
  }
  .button-grid .button {
    flex: 1;
    font-size: 10px;
    padding: 7px;
  }
  .segmented {
    display: flex;
    gap: 3px;
    background: #18181b;
    border: 1px solid #3f3f46;
    border-radius: 7px;
    padding: 4px;
    margin-bottom: 20px;
  }
  .segmented > button {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 5px;
    flex: 1;
    background: none;
    border: 0;
    border-radius: 4px;
    padding: 7px;
    font-size: 10px;
    color: #a1a1aa;
  }
  .segmented > button.active {
    background: #3f3f46;
    color: #bfdbfe;
  }
  .tool-explanation {
    display: flex;
    flex-direction: column;
    align-items: center;
    text-align: center;
    gap: 7px;
    padding: 20px 12px;
    color: #60a5fa;
  }
  .tool-explanation h3 {
    font-size: 13px;
    color: #bfdbfe;
    margin: 5px 0 0;
  }
  .tool-explanation p {
    font-size: 11px;
    line-height: 1.7;
    color: #a1a1aa;
    margin: 0 0 15px;
  }
  .section-heading {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 7px;
    margin-bottom: 11px;
  }
  .section-heading strong {
    font-size: 11px;
    color: #d4d4d8;
  }
  .format-card {
    display: flex;
    gap: 13px;
    padding: 14px;
    border: 1px solid #1d4ed8;
    border-radius: 7px;
    background: #27272a;
    margin: 3px 0 18px;
  }
  .format-card :global(svg) {
    flex: none;
    color: #93c5fd;
  }
  .format-card strong {
    font-size: 11px;
    color: #bfdbfe;
  }
  .format-card p {
    font-size: 10px;
    line-height: 1.65;
    color: #60a5fa;
    margin: 5px 0 0;
  }
  .preflight-card {
    border: 1px solid #755b3366;
    background: #49331b1c;
    border-radius: 7px;
    padding: 13px;
    margin: 18px 0;
    color: #eab308;
    font-size: 10px;
    line-height: 1.6;
  }
  .preflight-card .section-heading {
    justify-content: flex-start;
  }
  .preflight-card strong {
    color: #fbbf24;
  }
  .preflight-card ul {
    padding-left: 16px;
    margin: 8px 0;
  }
  .preflight-card li {
    margin: 6px 0;
  }
  .preflight-card.ready {
    background: #17433822;
    border-color: #52525b;
    color: #a1a1aa;
  }
  .preflight-card.ready strong {
    color: #86efac;
  }
  .preflight-card details {
    margin-top: 10px;
    font-size: 10px;
    color: #a1a1aa;
  }
  .preflight-check {
    display: flex;
    gap: 7px;
    align-items: center;
    padding: 4px 0;
  }
  .export-alternatives .button-grid {
    flex-wrap: wrap;
  }
  .export-alternatives .button {
    min-height: 44px;
    flex-basis: 130px;
    white-space: normal;
  }
  .export-result-notes { margin: 16px 0; padding: 12px; border: 1px solid #71717a; border-radius: 6px; color: #fbbf24; background: #382e222e; font-size: 11px; line-height: 1.7; overflow-wrap: anywhere; }
  .export-result-notes h3 { margin: 0; color: #fcd34d; font-size: 12px; }
  .export-result-notes p { margin: 8px 0; }
  .export-result-notes ul { padding-left: 18px; margin: 8px 0 0; }
  .export-result-notes li + li { margin-top: 7px; }
  .muted {
    color: #a1a1aa;
    font-size: 10px;
    line-height: 1.6;
  }
  .workspace-status {
    min-height: 30px;
    display: flex;
    gap: 9px;
    align-items: center;
    padding: 5px 17px;
    flex: none;
    background: #18181b;
    border-top: 1px solid var(--line);
    font-size: 9px;
    color: #a1a1aa;
  }
  .cancel-import, .cancel-export {
    min-height: 44px;
    padding: 0 12px;
    white-space: nowrap;
  }
  .status-dot {
    background: #22c55e;
    width: 5px;
    height: 5px;
    border-radius: 50%;
    flex: none;
  }
  .status-dot.working {
    background: #60a5fa;
    box-shadow: 0 0 6px #6ca8ff66;
  }
  .status-message {
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    max-width: 60%;
  }
  .workspace-status > span:not(.status-message) {
    white-space: nowrap;
  }
  .status-divider {
    height: 10px;
    width: 1px;
    background: #3f3f46;
  }
  .approved-stat {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    color: #a1a1aa;
  }
  .attention-stat {
    color: #eab308;
  }
  .loading-state {
    flex: 1;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    color: #60a5fa;
    gap: 12px;
  }
  .loading-state h2 {
    font-size: 17px;
    font-weight: 500;
  }
  .loading-state p {
    font-size: 12px;
    color: #a1a1aa;
  }
  .welcome {
    display: flex;
    flex-direction: column;
    align-items: center;
    flex: 1;
    min-height: 0;
    overflow: auto;
    padding: 44px 35px 22px;
    background: radial-gradient(ellipse at 50% 15%, #223d5a33, transparent 65%);
  }
  .welcome-main {
    display: flex;
    flex-direction: column;
    align-items: center;
    text-align: center;
    max-width: 620px;
  }
  .welcome-icon {
    background: #27272a;
    border: 1px solid #52525b;
    border-radius: 17px;
    width: 76px;
    height: 76px;
    display: grid;
    place-items: center;
    color: #93c5fd;
    margin-bottom: 26px;
    box-shadow: 0 8px 35px #122d4f66;
  }
  .welcome .eyebrow {
    font-size: 9px;
    color: #60a5fa;
    letter-spacing: 1.6px;
  }
  .welcome h1 {
    font-size: 36px;
    line-height: 1.2;
    letter-spacing: -1.3px;
    font-weight: 600;
    color: #dbeafe;
    margin: 16px 0 10px;
  }
  .welcome-main > p {
    font-size: 12px;
    line-height: 1.9;
    color: #a1a1aa;
    margin: 0 0 23px;
  }
  .supported {
    font-size: 9px;
    color: #71717a;
    margin-top: 12px;
    letter-spacing: 0.5px;
  }
  .welcome-steps {
    width: 100%;
    max-width: 850px;
    display: grid;
    grid-template-columns: repeat(3, 1fr);
    gap: 15px;
    margin-top: 42px;
  }
  .welcome-steps article {
    position: relative;
    padding: 22px 20px;
    border: 1px solid #3f3f46;
    background: #16243966;
    border-radius: 9px;
    color: #3b82f6;
  }
  .welcome-steps h3 {
    font-size: 12px;
    font-weight: 600;
    color: #d4d4d8;
    margin: 15px 0 7px;
  }
  .welcome-steps p {
    font-size: 10px;
    line-height: 1.8;
    color: #a1a1aa;
    margin: 0;
  }
  .step-number {
    position: absolute;
    top: 20px;
    right: 20px;
    color: #52525b;
    font-size: 10px;
    font-variant-numeric: tabular-nums;
  }
  .welcome-footer {
    display: flex;
    align-items: center;
    gap: 7px;
    margin-top: 26px;
    font-size: 9px;
    color: #71717a;
  }
  .capability-note {
    display: flex;
    gap: 8px;
    font-size: 10px;
    line-height: 1.6;
    color: #eab308;
    background: #523b1e44;
    border: 1px solid #74572c66;
    border-radius: 6px;
    padding: 10px 12px;
    margin-top: 20px;
    text-align: left;
  }
  .capability-note :global(svg) {
    flex: none;
  }
  .project-menu {
    position: absolute;
    z-index: 20;
    right: 25px;
    top: 68px;
    width: 350px;
    max-height: min(500px, calc(100% - 80px));
    overflow: auto;
    overscroll-behavior: contain;
    padding: 15px;
    background: #27272a;
    border: 1px solid #52525b;
    border-radius: 9px;
    box-shadow: 0 14px 40px #0008;
  }
  .project-option {
    display: flex;
    align-items: center;
    gap: 11px;
    width: 100%;
    text-align: left;
    background: #27272a;
    border: 1px solid #3f3f46;
    border-radius: 6px;
    padding: 12px;
    color: #93c5fd;
    margin-top: 8px;
  }
  .project-option > span {
    flex: 1;
    min-width: 0;
  }
  .project-option strong {
    display: block;
    font-size: 11px;
    color: #bfdbfe;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .project-option small {
    display: block;
    font-size: 9px;
    margin-top: 5px;
    color: #a1a1aa;
  }
  .workspace-dialog::backdrop {
    background: #030b15b8;
    backdrop-filter: blur(3px);
  }
  .workspace-dialog {
    /* Top-layer dialogs are positioned against the viewport, outside the
       padded workspace. Inset all four edges before centering the dialog. */
    position: fixed;
    inset: calc(12px + env(safe-area-inset-top, 0px))
      calc(12px + env(safe-area-inset-right, 0px))
      calc(12px + env(safe-area-inset-bottom, 0px))
      calc(12px + env(safe-area-inset-left, 0px));
    margin: auto;
    width: 430px;
    height: fit-content;
    max-width: calc(100vw - 24px - env(safe-area-inset-left, 0px) - env(safe-area-inset-right, 0px));
    max-height: calc(100vh - 24px - env(safe-area-inset-top, 0px) - env(safe-area-inset-bottom, 0px));
    max-height: calc(100dvh - 24px - env(safe-area-inset-top, 0px) - env(safe-area-inset-bottom, 0px));
    box-sizing: border-box;
    overflow: auto;
    overscroll-behavior: contain;
    overflow-wrap: anywhere;
    background: #27272a;
    border: 1px solid #52525b;
    border-radius: 11px;
    padding: 24px;
    color: #dbeafe;
    box-shadow: 0 25px 80px #0009;
    font-family: Inter, system-ui, sans-serif;
  }
  .workspace-dialog h2 {
    font-size: 18px;
    margin: 0 0 14px;
  }
  .workspace-dialog p {
    font-size: 12px;
    line-height: 1.8;
    color: #93c5fd;
    margin: 0 0 20px;
  }
  .workspace-dialog.ocr-preview-dialog {
    box-sizing: border-box;
    width: min(800px, calc(100vw - 32px));
    max-height: calc(100dvh - 32px);
    overflow: auto;
  }
  .ocr-preview-comparison {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 14px;
  }
  .ocr-preview-comparison label {
    display: flex;
    flex-direction: column;
    gap: 8px;
    min-width: 0;
    color: #dbeafe;
    font-size: 12px;
    font-weight: 600;
  }
  .ocr-preview-comparison textarea {
    box-sizing: border-box;
    width: 100%;
    min-height: 200px;
    resize: vertical;
    border: 1px solid #52525b;
    border-radius: 5px;
    padding: 10px;
    background: #18181b;
    color: #dbeafe;
    font: inherit;
    font-weight: 400;
    line-height: 1.65;
  }
  .ocr-preview-warnings {
    color: #fbbf24;
    font-size: 12px;
    line-height: 1.65;
    padding-left: 20px;
  }
  .workspace-dialog .ocr-preview-note {
    margin-top: 14px;
    margin-bottom: 0;
  }
  @media (max-width: 600px) {
    .ocr-preview-comparison {
      grid-template-columns: minmax(0, 1fr);
    }
    .ocr-preview-comparison textarea {
      min-height: 130px;
    }
    .ocr-preview-dialog .dialog-actions {
      flex-wrap: wrap;
    }
  }
  .dialog-actions {
    display: flex;
    flex-wrap: wrap;
    justify-content: flex-end;
    gap: 8px;
    margin-top: 24px;
  }
  .workspace-dialog button { min-height: 44px; }
  .workspace-dialog .icon-button { min-width: 44px; }
  .shortcuts {
    display: grid;
    grid-template-columns: 1fr auto;
    gap: 13px 20px;
    font-size: 11px;
    margin: 20px 0;
  }
  .shortcuts dt {
    color: #93c5fd;
  }
  .shortcuts dd {
    margin: 0;
    color: #bfdbfe;
    background: #18181b;
    padding: 3px 7px;
    border: 1px solid #3f3f46;
    border-radius: 4px;
    font-size: 10px;
  }
  :global(.spin) {
    animation: ocr-spin 1.3s linear infinite;
  }
  @keyframes ocr-spin {
    to {
      transform: rotate(360deg);
    }
  }
  @media (min-width: 1600px) {
    .work-area {
      grid-template-columns: 234px minmax(400px, 1fr) 430px;
    }
    .work-area.rail-hidden {
      grid-template-columns: minmax(400px, 1fr) 430px;
    }
    .inspector-tabs > button {
      font-size: 11px;
    }
    .region-card textarea {
      font-size: 13px !important;
    }
  }
  @media (max-width: 1200px) {
    .work-area {
      grid-template-columns: 185px minmax(240px, 1fr) 340px;
    }
    .work-area.rail-hidden {
      grid-template-columns: minmax(240px, 1fr) 340px;
    }
    .save-state {
      display: none;
    }
    .workspace-header {
      padding: 13px 16px;
    }
    .command-bar {
      padding: 9px 12px;
      gap: 4px;
    }
    .command-bar .button {
      font-size: 10px;
      padding: 7px 8px;
    }
    .image-toolbar {
      gap: 4px;
      padding: 8px 5px;
    }
    .image-toolbar .icon-button {
      padding: 4px;
      min-width: 24px;
    }
    .image-toolbar select {
      max-width: 105px;
    }
    .thumbnail {
      width: 43px;
      height: 65px;
    }
    .page-card {
      gap: 5px;
      padding: 8px 4px;
    }
    .page-card-main {
      gap: 7px;
    }
    .page-filename {
      font-size: 8px;
    }
    .region-card textarea {
      font-size: 11px !important;
    }
  }
  @media (max-width: 980px) {
    .work-area {
      grid-template-columns: 155px minmax(220px, 1fr) 310px;
    }
    .work-area.rail-hidden {
      grid-template-columns: minmax(220px, 1fr) 310px;
    }
    .optional-label {
      display: none;
    }
    .workspace-title input {
      font-size: 16px !important;
    }
    .page-card {
      padding: 7px 3px;
      min-height: 85px;
    }
    .thumbnail {
      width: 35px;
      height: 55px;
    }
    .page-card-info > strong {
      font-size: 10px;
    }
    .page-state {
      font-size: 8px;
    }
    .page-card-main {
      gap: 5px;
    }
    .page-checkbox {
      margin-top: 20px !important;
    }
    .image-toolbar .tool-group + .tool-group {
      padding-left: 2px;
    }
    .image-toolbar select {
      max-width: 87px;
      font-size: 9px !important;
    }
    .image-toolbar .button.small {
      font-size: 9px;
      padding: 4px 5px;
    }
    .inspector-tabs {
      padding: 0 3px;
    }
    .inspector-tabs > button {
      font-size: 9px;
      gap: 3px;
    }
    .inspector-tabs :global(svg) {
      width: 12px;
    }
    .panel-scroll {
      padding: 14px;
    }
    .approval-bar {
      padding: 9px 6px;
    }
    .approval-bar .button {
      font-size: 9px;
      padding: 7px;
    }
    .queue-label > span {
      display: none;
    }
  }
  @media (max-width: 780px) {
    .workspace-header {
      gap: 8px;
      min-height: 66px;
      padding: 11px 12px;
    }
    .workspace-mark {
      width: 33px;
      height: 33px;
      border-radius: 8px;
    }
    .eyebrow {
      font-size: 8px;
      letter-spacing: 1px;
    }
    .workspace-title input {
      font-size: 15px !important;
    }
    .header-actions {
      gap: 2px;
    }
    .header-actions .button {
      font-size: 10px;
      padding: 5px 7px;
    }
    .header-actions > .shortcuts-button {
      display: none;
    }
    .command-bar {
      flex-wrap: wrap;
      row-gap: 6px;
    }
    .command-separator {
      display: none;
    }
    .command-bar .toolbar-spacer {
      min-width: 0;
    }
    .command-bar .button {
      font-size: 9px;
      min-height: 29px;
    }
    .command-bar > .button.subtle {
      display: none;
    }
    .work-area,
    .work-area.rail-hidden {
      grid-template-columns: minmax(190px, 1fr) minmax(270px, 44%);
    }
    .page-rail {
      display: none;
    }
    .document-pane .image-toolbar {
      overflow-x: auto;
    }
    .image-toolbar .toolbar-spacer {
      display: none;
    }
    .image-toolbar select {
      max-width: 82px;
    }
    .work-area .document-pane .image-toolbar > .button {
      display: none;
    }
    .image-toolbar {
      justify-content: space-between;
    }
    .queue-bar {
      padding: 8px 10px;
      gap: 7px;
      font-size: 9px;
    }
    .queue-bar progress {
      max-width: 130px;
    }
    .queue-label strong {
      font-size: 9px;
    }
    .workspace-status {
      font-size: 8px;
      padding: 6px 8px;
      gap: 6px;
    }
    .status-message {
      max-width: 42%;
    }
    .status-divider {
      display: none;
    }
    .welcome {
      padding: 30px 20px 20px;
    }
    .welcome h1 {
      font-size: 28px;
    }
    .welcome-steps {
      gap: 8px;
    }
    .welcome-steps article {
      padding: 18px 13px;
    }
    .project-menu {
      right: 12px;
      width: min(350px, calc(100% - 24px));
    }
  }
  @media (max-width: 560px) {
    .work-area,
    .work-area.rail-hidden {
      display: flex;
      flex-direction: column;
      overflow: auto;
    }
    .document-pane {
      min-height: 300px;
      height: 42vh;
      flex: none;
    }
    .inspector {
      min-height: 390px;
      flex: 1;
      border-left: 0;
      border-top: 1px solid #3f3f46;
      overflow: visible;
    }
    .region-list {
      min-height: 220px;
      max-height: 45vh;
    }
    .inspector-tabs {
      position: sticky;
      top: 0;
      z-index: 3;
    }
    .panel-scroll {
      overflow: visible;
    }
    .workspace-header .header-actions .button {
      padding: 5px;
      font-size: 9px;
    }
    .header-actions .button :global(svg) {
      width: 13px;
    }
    .workspace-title input {
      font-size: 13px !important;
    }
    .workspace-mark {
      display: none;
    }
    .command-bar {
      padding: 7px 8px;
    }
    .command-bar .button {
      font-size: 9px;
      padding: 6px;
    }
    .command-bar .undo-group {
      padding-right: 3px;
    }
    .command-bar .undo-group .icon-button {
      padding: 4px;
      min-width: 24px;
    }
    .command-bar .button.export-button {
      margin-left: auto;
    }
    .workspace-status .approved-stat {
      display: none;
    }
    .welcome-steps {
      grid-template-columns: 1fr;
      margin-top: 24px;
      max-width: 400px;
    }
    .welcome-steps article {
      padding: 16px 18px;
    }
    .welcome-steps article :global(svg) {
      float: left;
      margin: 2px 13px 25px 0;
    }
    .welcome-steps h3 {
      margin: 0 0 5px;
    }
    .welcome-icon {
      width: 58px;
      height: 58px;
      margin-bottom: 20px;
    }
    .welcome h1 {
      font-size: 26px;
    }
    .welcome-main > p {
      font-size: 11px;
    }
    .welcome-footer {
      font-size: 8px;
      text-align: center;
    }
    .queue-bar progress {
      display: none;
    }
    .queue-label {
      flex: 1;
    }
    .queue-actions {
      gap: 3px;
    }
    .queue-actions .button {
      font-size: 9px;
    }
    .image-toolbar {
      justify-content: flex-start;
      gap: 10px;
    }
    .image-toolbar .tool-group + .tool-group {
      padding-left: 7px;
    }
    .image-toolbar select {
      max-width: 140px;
    }
    .image-footer {
      height: 35px;
    }
  }
  .camera-input { display: none; }
  .mobile-workspace-nav { display: none; }
  .photo-tool-toggle { display: flex; align-items: center; gap: 10px; padding: 5px 10px; border-bottom: 1px solid var(--line); background: #18181b; flex: none; }
  .photo-tool-toggle > span { font-size: 10px; color: var(--muted); }
  .photo-focus-controls { display: none; }
  .batch-recovery { display: flex; align-items: center; gap: 10px; padding: 9px 16px; background: #27272a; border-bottom: 1px solid #52525b; flex: none; }
  .batch-recovery > div { flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 4px; }
  .batch-recovery span { color: #d4d4d8; font-size: 10px; }
  @media (max-width: 780px) {
    .mobile-workspace-nav { display: grid; grid-template-columns: 1fr 1fr 1fr; border-bottom: 1px solid #52525b; background: #18181b; flex: none; }
    .mobile-workspace-nav button { display: flex; align-items: center; justify-content: center; gap: 6px; min-height: 48px; border: 0; border-bottom: 3px solid transparent; background: transparent; color: #a1a1aa; font-size: 11px; padding: 8px 3px; }
    .mobile-workspace-nav button.active { color: #dbeafe; background: #3f3f46; border-bottom-color: #60a5fa; }
    .mobile-workspace-nav span { border-radius: 4px; padding: 1px 4px; background: #3f3f46; }
    .work-area, .work-area.rail-hidden { display: flex; flex-direction: column; overflow: hidden; min-height: 0; }
    .work-area .page-rail, .work-area .document-pane, .work-area .inspector { display: none; height: 100%; min-height: 0; flex: 1; min-width: 0; overflow: hidden; }
    .work-area[data-mobile-view="pages"] .page-rail, .work-area[data-mobile-view="image"] .document-pane, .work-area[data-mobile-view="tools"] .inspector { display: flex; }
    .work-area .inspector { border: 0; }
    .work-area .document-pane :global(.canvas-shell) { min-height: 120px; flex: 1; }
    .work-area :global(.canvas-hint) { padding: 0 10px; }
    .work-area :global(.viewport) { padding: 24px; }
    .region-list { min-height: 0; max-height: none; flex: 1; }
    .panel-scroll { overflow: auto; min-height: 0; }
    .page-card { padding: 10px 12px; min-height: 100px; }
    .page-card-info > strong { font-size: 13px; }
    .page-filename, .page-state { font-size: 11px; }
    .thumbnail { width: 48px; height: 70px; }
    .page-card-main { gap: 12px; }
    .page-checkbox { width: 22px !important; height: 22px !important; margin: 22px 9px 0 0 !important; }
    .ocr-workspace .button, .ocr-workspace .icon-button, .ocr-workspace .text-button { min-height: 44px; }
    .ocr-workspace .icon-button { min-width: 44px; padding: 8px; }
    .command-bar .undo-group .icon-button { min-width: 44px; }
    .workspace-header .header-actions .button, .workspace-header .header-actions .icon-button { min-height: 44px; min-width: 44px; }
    .command-bar { column-gap: 5px; row-gap: 4px; padding: 5px 8px; }
    .command-bar .button { font-size: 10px; }
    .command-bar .undo-group { margin-left: auto; }
    .command-bar .toolbar-spacer { display: none; }
    .image-toolbar { min-height: 48px; flex: none; overflow-x: auto; justify-content: flex-start; gap: 6px; }
    .image-toolbar .tool-group { flex: none; }
    .work-area .document-pane .image-toolbar > .button { display: inline-flex; flex: none; }
    .image-toolbar select { max-width: 140px; min-height: 44px; font-size: 12px !important; }
    .image-footer { height: auto; min-height: 48px; flex: none; gap: 2px; padding: 3px 5px; }
    .image-footer .icon-button { min-width: 36px; }
    .image-footer select { max-width: 92px; }
    .photo-tool-toggle { padding: 3px 10px; }
    .photo-tool-toggle .button { min-height: 40px; }
    .inspector-tabs > button { min-height: 48px; font-size: 11px; }
    .batch-recovery { flex-wrap: wrap; padding: 8px 10px; gap: 5px; }
    .batch-recovery > div { flex-basis: 100%; }
    .queue-bar { flex-wrap: wrap; }
    .queue-label { min-width: 0; }
    .queue-label strong { font-size: 11px; }
    .queue-actions .button { min-height: 40px; }
    .approval-bar .button { font-size: 11px; }
    .ocr-workspace :global(textarea), .ocr-workspace :global(input:not([type="checkbox"]):not([type="range"])), .ocr-workspace :global(select) { font-size: 16px; }
    .workspace-title input { font-size: 14px !important; }
    .photo-tool-toggle > span { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
    .ocr-workspace.photo-focusing .workspace-header,
    .ocr-workspace.photo-focusing .command-bar,
    .ocr-workspace.photo-focusing .mobile-workspace-nav,
    .ocr-workspace.photo-focusing .image-toolbar,
    .ocr-workspace.photo-focusing .photo-tool-toggle,
    .ocr-workspace.photo-focusing .document-pane :global(.photo-preparation) { display: none; }
    .photo-focusing .photo-focus-controls { display: block; flex: none; background: #27272a; border-bottom: 1px solid #52525b; padding: 8px; }
    .photo-focus-actions { display: flex; gap: 8px; }
    .photo-focus-actions .button { flex: 1; min-width: 0; min-height: 44px; padding: 8px 5px; font-size: 11px; white-space: normal; }
    .photo-focus-error { margin: 7px 4px 0; color: #fcd34d; line-height: 1.4; font-size: 11px; }
    .photo-focus-coordinates { margin-top: 4px; }
    .photo-focus-coordinates summary { min-height: 44px; padding: 12px 5px; color: #bfdbfe; cursor: pointer; font-size: 11px; }
    .photo-focus-coordinate-grid { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 8px; max-height: clamp(132px, 30dvh, 250px); overflow: auto; overscroll-behavior: contain; padding: 2px 2px 8px; }
    .photo-focus-coordinate-grid fieldset { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 6px; margin: 0; padding: 5px 7px 8px; border: 1px solid #52525b; border-radius: 6px; min-width: 0; }
    .photo-focus-coordinate-grid legend { padding: 0 4px; color: #bfdbfe; font-size: 11px; }
    .photo-focus-coordinate-grid label { display: flex; flex-direction: column; gap: 3px; color: var(--muted); font-size: 10px; min-width: 0; }
    .photo-focus-coordinate-grid input { width: 100%; min-height: 44px; padding: 6px !important; font-size: 16px !important; }
    .photo-focusing .image-footer .page-navigation,
    .photo-focusing .image-footer .toolbar-spacer { display: none; }
    .photo-focusing .image-footer { justify-content: center; gap: 8px; }
    .ocr-workspace.photo-focusing .image-footer .icon-button { min-width: 44px; }
    .photo-focusing .image-footer select { min-height: 44px; min-width: 120px; max-width: 140px; font-size: 14px !important; }
  }
  @media (prefers-reduced-motion: reduce) {
    .button {
      transition: none;
    }
    :global(.spin) {
      animation: none;
    }
  }
  /* One compact top row: project title, save state and all commands. */
  .workspace-top { display: flex; flex-wrap: wrap; align-items: center; border-bottom: 1px solid var(--line); flex: none; }
  .workspace-top > .workspace-header { min-height: 0; padding: 8px 14px; border-bottom: 0; flex: 1 1 auto; }
  .workspace-top > .command-bar { border-bottom: 0; flex: 1 1 auto; padding-top: 6px; padding-bottom: 6px; }
  .workspace-top .workspace-mark, .workspace-top .eyebrow { display: none; }
  .image-toolbar .photo-tool-toggle { padding: 0; border: 0; background: none; }
  .image-toolbar .photo-tool-toggle > span { display: none; }
  .queue-bar { position: relative; }
  .queue-bar-dismiss { position: absolute; right: 8px; top: 50%; transform: translateY(-50%); background: none; border: 0; color: #a1a1aa; font-size: 16px; cursor: pointer; }
  @media (max-width: 780px) {
    .workspace-top { display: contents; }
  }
  /* Compact review bar: view switch, notes and tools in one row. */
  .review-bar { display: flex; align-items: center; gap: 4px; padding: 6px 12px; border-bottom: 1px solid var(--line); flex: none; }
  .review-bar .review-view-tabs { border: 0; padding: 0; margin: 0; }
  .review-bar { flex-wrap: wrap; row-gap: 2px; }
  .review-bar .review-view-tabs { gap: 0; }
  .review-bar .review-view-tabs button { padding: 4px 6px; }
  .review-bar .icon-button { width: 28px; height: 28px; }
  .review-bar .recognition-notes { position: relative; margin: 0; border: 0; background: none; padding: 0; }
  .review-bar .recognition-notes summary { display: flex; align-items: center; gap: 3px; color: #fbbf24; font-size: 11px; cursor: pointer; list-style: none; padding: 4px 6px; }
  .review-bar .recognition-notes summary::-webkit-details-marker { display: none; }
  .review-bar .recognition-notes ul { position: absolute; right: 0; top: 100%; z-index: 20; width: 280px; margin: 4px 0 0; padding: 8px 10px 8px 24px;
    background: #27272a; border: 1px solid #3f3f46; border-radius: 6px; font-size: 11px; color: #e4e4e7; box-shadow: 0 6px 20px #0008; }
  .needs-attention { padding: 6px 14px 0; }
  .workspace-status .queue-bar { display: flex; align-items: center; gap: 8px; padding: 0; margin: 0; border: 0; background: none; min-height: 0; }
  .workspace-status .queue-bar progress { width: 110px; height: 4px; }
  .workspace-status .queue-label { display: flex; align-items: center; gap: 5px; }
  .workspace-status .queue-actions { display: flex; gap: 4px; }
  .visually-hidden { position: absolute; width: 1px; height: 1px; padding: 0; margin: -1px; overflow: hidden; clip: rect(0 0 0 0); white-space: nowrap; border: 0; }
  .image-toolbar { flex-wrap: nowrap; overflow-x: auto; }
  .image-toolbar .icon-button { flex: none; }
  .image-toolbar select { max-width: 150px; }
</style>
