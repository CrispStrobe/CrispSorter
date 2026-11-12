import { flog } from './log';
import { ocrEngineLabel } from './ocr/engineCatalog';

export type OperationName = 'ocr.session' | 'ocr.recognition' | 'ocr.worker' | 'ocr.pdf'
  | 'ocr.import' | 'ocr.export' | 'ocr.prepare' | 'ocr.transform' | 'ocr.specialist'
  | 'ocr.batch' | 'ocr.checkpoint' | 'settings.save' | 'settings.ocr' | 'settings.model'
  | 'ocr.models' | 'ocr.reindex' | 'ocr.project' | 'document.extract'
  | 'search.request' | 'search.summary' | 'search.export' | 'search.cancel' | 'batch.processing'
  | 'batch.sort' | 'batch.index_repair' | 'batch.replace'
  | 'pdf.open' | 'pdf.close' | 'pdf.edit' | 'pdf.undo' | 'pdf.redo' | 'pdf.save'
  | 'pdf.export' | 'pdf.thumbnails' | 'pdf.render' | 'pdf.validation' | 'pdf.tool'
  | 'pdf.print' | 'pdf.share' | 'pdf.external' | 'pdf.form' | 'pdf.merge'
  | 'pdf.split' | 'pdf.encrypt' | 'pdf.decrypt' | 'pdf.sanitise' | 'pdf.compress'
  | 'pdf.kindle' | 'pdf.annotations' | 'pdf.images' | 'pdf.signatures'
  | 'pdf.linearize' | 'pdf.repair' | 'pdf.pdfa' | 'pdf.drop'
  | 'document.convert' | 'document.capabilities' | 'document.docx'
  | 'docx.check' | 'docx.properties' | 'docx.headings' | 'docx.restyle'
  | 'docx.notes' | 'docx.quotes' | 'docx.clean' | 'docx.footnotes'
  | 'batch.duplicate_scan' | 'batch.duplicate_cleanup' | 'batch.review_edit' | 'batch.index_add'
  | 'chat.request'
  | 'settings.test_connection' | 'settings.refresh_models'
  | 'models.download' | 'models.delete' | 'models.pull'
  | 'app.capabilities' | 'app.log_export' | 'app.provider_pick';
export type OperationPhase = 'queued' | 'loading' | 'ready' | 'reused' | 'disposed'
  | 'dispatching' | 'recognizing' | 'rendering' | 'text_ready' | 'page_completed'
  | 'checking' | 'saving' | 'saved' | 'committing' | 'published' | 'progress'
  | 'paused' | 'resumed' | 'cancel_requested' | 'cancel_refused' | 'discarded'
  | 'checkpoint_failed' | 'cleanup_failed' | 'capability_failed' | 'warning'
  | 'settings_failed' | 'index_pending' | 'fallback' | 'awaiting_models'
  | 'analyzing' | 'analyzed' | 'extraction_failed' | 'analysis_failed';

/** Only operational metadata belongs in the app log. Never pass source text,
 * queries, prompts, model paths, URLs, credentials or raw error messages. */
export interface OperationFields {
  engine?: string; language?: string; format?: string;
  backend?: 'native' | 'browser' | 'local' | 'remote';
  kind?: 'image' | 'pdf' | 'archive' | 'mail' | 'presentation' | 'table' | 'fields' | 'formula' | 'copy' | 'move' | 'script';
  page?: number; pages?: number; total?: number; bytes?: number; characters?: number;
  regions?: number; warnings?: number; results?: number; completed?: number;
  failed?: number; pending?: number; count?: number; revision?: number;
  cpuBudget?: boolean; forced?: boolean; partial?: boolean; saved?: boolean;
  reused?: boolean; nativeContinues?: boolean; pipeline?: boolean; stages?: number;
}

const numeric = new Set(['page', 'pages', 'total', 'bytes', 'characters', 'regions', 'warnings',
  'results', 'completed', 'failed', 'pending', 'count', 'revision', 'stages']);
const boolean = new Set(['cpuBudget', 'forced', 'partial', 'saved', 'reused', 'nativeContinues', 'pipeline']);
const formats = new Set(['text', 'txt', 'markdown', 'md', 'html', 'json', 'csv', 'pdf', 'pdfa', 'hocr', 'alto', 'docx', 'xlsx']);
const kinds = new Set(['image', 'pdf', 'archive', 'mail', 'presentation', 'table', 'fields', 'formula', 'copy', 'move', 'script']);
let sequence = 0;

function fieldsText(fields: OperationFields): string {
  const parts: string[] = [];
  for (const [key, value] of Object.entries(fields)) {
    if (numeric.has(key) && typeof value === 'number' && Number.isFinite(value) && value >= 0) {
      parts.push(`${key}=${Math.round(value)}`);
    } else if (boolean.has(key) && typeof value === 'boolean') parts.push(`${key}=${value}`);
    else if (key === 'engine' && typeof value === 'string') {
      parts.push(`engine=${value === 'tesseract_js' || ocrEngineLabel(value) !== value ? value : 'unknown'}`);
    } else if (key === 'language' && typeof value === 'string' && value.length <= 128
      && /^(?:auto|[a-z]{2,3}(?:[-_][a-zA-Z]{2,4})?)(?:\+[a-z]{2,3}(?:[-_][a-zA-Z]{2,4})?)*$/.test(value)) parts.push(`language=${value}`);
    else if (key === 'backend' && ['native', 'browser', 'local', 'remote'].includes(String(value))) parts.push(`backend=${value}`);
    else if (key === 'kind' && kinds.has(String(value))) parts.push(`kind=${value}`);
    else if (key === 'format' && formats.has(String(value))) parts.push(`format=${value}`);
  }
  return parts.length ? ` ${parts.join(' ')}` : '';
}

export function isOperationCancelled(error: unknown): boolean {
  return error instanceof Error && error.name === 'AbortError';
}

/** A single terminal receipt prevents late results from relabelling cancelled
 * work as success. Logging cannot change an operation's result or error. */
export function beginOperation(name: OperationName, fields: OperationFields = {}) {
  const id = `fe-${Date.now().toString(36)}-${(++sequence).toString(36)}`;
  const started = performance.now();
  let terminal = false;
  const emit = (phase: string, extra: OperationFields, level: 'info' | 'warn' | 'error' | 'debug') => {
    const state = ['started', 'completed', 'failed', 'cancelled'].includes(phase) ? `phase=${phase}` : `phase=stage stage=${phase}`;
    try { flog(level, `${name} ${state} operation_id=${id} elapsed_ms=${Math.max(0, Math.round(performance.now() - started))}${fieldsText({ ...fields, ...extra })}`); }
    catch { /* A log observer cannot fail a completed write or mask its error. */ }
  };
  const finish = (phase: 'completed' | 'failed' | 'cancelled', extra: OperationFields, level: 'info' | 'warn' | 'error') => {
    if (terminal) return;
    terminal = true;
    emit(phase, extra, level);
  };
  emit('started', {}, 'info');
  return {
    id,
    stage(phase: OperationPhase, extra: OperationFields = {}, level: 'info' | 'warn' | 'error' | 'debug' = 'debug') {
      if (!terminal) emit(phase, extra, level);
    },
    complete(extra: OperationFields = {}) { finish('completed', extra, extra.partial ? 'warn' : 'info'); },
    fail(error?: unknown, extra: OperationFields = {}) {
      finish(isOperationCancelled(error) ? 'cancelled' : 'failed', extra, isOperationCancelled(error) ? 'info' : 'error');
    },
    cancel(extra: OperationFields = {}) { finish('cancelled', extra, 'info'); },
  };
}

export type OperationLog = ReturnType<typeof beginOperation>;

/** Share terminal handling while leaving publication/cancellation decisions at
 * the caller that owns the real receipt. Error values are rethrown unchanged. */
export async function runOperation<T>(
  name: OperationName,
  fields: OperationFields,
  run: (operation: OperationLog) => Promise<T>,
  receipt: (result: T) => OperationFields = () => ({}),
): Promise<T> {
  const operation = beginOperation(name, fields);
  try {
    const result = await run(operation);
    // Receipt formatting is also observability code: a malformed optional
    // response field must never turn a successful write into a retry.
    let fields: OperationFields = {};
    try { fields = receipt(result); } catch { operation.stage('warning', {}, 'warn'); }
    operation.complete(fields);
    return result;
  } catch (error) {
    operation.fail(error);
    throw error;
  }
}
