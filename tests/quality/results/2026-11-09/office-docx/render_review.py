"""Read-only render of production-generated OCR fixtures with the skill renderer."""
import importlib.util
import os
from pathlib import Path

temporary = Path('/workspace/scratch/82243b18126c/ocr-runtime-temp')
temporary.mkdir(exist_ok=True)
os.environ['TMPDIR'] = str(temporary)
source = '/home/agent/.codex/skills/builtins/documents/render_docx.py'
spec = importlib.util.spec_from_file_location('ocr_fixture_renderer', source)
renderer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(renderer)

def reader_environment(profile):
    # Keep the process HOME unchanged. LibreOffice receives an explicit isolated
    # UserInstallation and writable XDG configuration/cache paths from this run.
    environment = os.environ.copy()
    for name, directory in [('XDG_CONFIG_HOME', 'xdg_config'), ('XDG_CACHE_HOME', 'xdg_cache')]:
        target = Path(profile) / directory
        target.mkdir(exist_ok=True)
        environment[name] = str(target)
    return environment

renderer._build_lo_env = reader_environment
renderer.main()
