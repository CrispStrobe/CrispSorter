"""Replace known generated mobile build files without clearing their directories."""
from pathlib import Path


def directory(path):
    path = Path(path)
    if path.is_symlink() or (path.exists() and not path.is_dir()):
        raise ValueError('Mobile staging directory must be a regular directory')
    path.mkdir(parents=True, exist_ok=True)
    return path


def output_file(root, relative):
    """Keep generated writes out of symlinked files and child directories."""
    root, relative = Path(root), Path(relative)
    if relative.is_absolute() or '..' in relative.parts:
        raise ValueError('Generated mobile file must remain inside its staging directory')
    parent = directory(root)
    for part in relative.parts[:-1]:
        parent = directory(parent / part)
    path = root / relative
    if path.is_symlink() or (path.exists() and not path.is_file()):
        raise ValueError('Generated mobile output must be a regular file')
    return path
