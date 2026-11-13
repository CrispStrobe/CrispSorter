"""Conservative retention of build/test diagnostics without credential values.

Ambiguous credential-bearing output is withheld in full. This is a diagnostic
transport decision, separate from the raw test exit/count and source checks.
"""
import re
from urllib.parse import unquote, urlsplit

OMITTED = '<diagnostics withheld: sensitive or ambiguous content>\n'
SENSITIVE = r'password|passwd|token|secret|credential|authorization|api[\W_]*key|private[\W_]*key|cookie|username'


def sanitize_output(text, roots=()):
    # Decode common nested JSON/URI encodings for inspection, without assuming
    # that arbitrary compiler/native text forms a complete JSON document.
    value = re.sub(r'\x1b\[[0-?]*[ -/]*[@-~]', '', text)
    for _ in range(8):
        decoded = value.replace("\\\\", "\\")
        decoded = re.sub(r'\\u([0-9a-fA-F]{4})', lambda m: chr(int(m[1], 16)), decoded)
        decoded = decoded.replace(r'\/', '/').replace(r'\"', '"').replace(r"\'", "'")
        decoded = decoded.replace(r'\n', '\n').replace(r'\r', '\r').replace(r'\t', '\t')
        decoded = unquote(decoded)
        if decoded == value:
            break
        value = decoded
    else:
        return OMITTED, 'withheld-sensitive-or-ambiguous'
    if re.search(r'\\u[0-9a-fA-F]|%[0-9a-fA-F]{2}|\x00|\x1b', value):
        return OMITTED, 'withheld-sensitive-or-ambiguous'
    # Quoted keys and header/pair labels catch nested JSON and name/value forms,
    # including malformed/truncated fragments. Never guess where values end.
    quoted = r'''["'][^"'\r\n]*(?:''' + SENSITIVE + r''')[^"'\r\n]*["']'''
    assignment = r'\b[\w.-]*(?:' + SENSITIVE + r')[\w.-]*\s*[:=]'
    if (re.search(quoted, value, re.I) or re.search(assignment, value, re.I)
            or re.search(r'\bbearer\b|BEGIN\s+[^\r\n]*PRIVATE\s+KEY', value, re.I)
            or re.search(r'\b(?:gh[pousr]_[A-Za-z0-9_]+|github_pat_[A-Za-z0-9_]+)\b', value)):
        return OMITTED, 'withheld-sensitive-or-ambiguous'
    for match in re.finditer(r'''\b[a-z][a-z0-9+.-]*://[^\s<>"']+''', value, re.I):
        try:
            url = urlsplit(match[0])
            if not url.hostname or url.username is not None or url.password is not None or url.query or url.fragment:
                return OMITTED, 'withheld-sensitive-or-ambiguous'
        except ValueError:
            return OMITTED, 'withheld-sensitive-or-ambiguous'
    for root in sorted({str(x) for x in roots if x}, key=len, reverse=True):
        if root != '/':
            value = value.replace(root, '<runner-path>')
    value = re.sub(r'\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b',
                   '<operation-id>', value, flags=re.I)
    return value, 'sanitized'
