#!/usr/bin/env python3
"""Generate a geometry fixture from exact application CSS and viewport sources."""
import argparse
import hashlib
import html
import json
import re
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
BASELINE = '646dbc486438d0e8955af193b30e6382632cdf45'
PATHS = ['src/app.html', 'src/app.css', 'src/routes/+page.svelte']

BODY = '''<div class="app-shell">
<nav class="main-nav"><div class="nav-top"><div class="logo-area">CrispSorter</div><button class="nav-item">Desktop control</button></div></nav>
<main class="main-content"><div class="content-area"><h1>Shell geometry fixture</h1><p>Source CSS only. No OCR application commands or document providers.</p></div></main>
<nav class="mobile-nav" aria-label="Application navigation">__BUTTONS__
<div id="tools-menu" class="mobile-tools-menu" role="menu" aria-label="Tools menu" style="display:none">
<button>Translate</button><button>OCR</button><button>PDF tools</button><button>Drives</button><button>History</button>
</div></nav></div>'''

SCRIPT = '''
function rect(element) { const r=element.getBoundingClientRect(); return {x:r.x,y:r.y,width:r.width,height:r.height}; }
window.measureGeometry = () => {
 const shell=document.querySelector('.app-shell'), style=getComputedStyle(shell), menu=document.querySelector('#tools-menu');
 const data={variant:document.body.dataset.variant,viewport:document.querySelector('meta[name=viewport]').content,
  innerWidth,innerHeight,visualViewport:{width:visualViewport.width,height:visualViewport.height,scale:visualViewport.scale},
  shell:rect(shell),main:rect(document.querySelector('.main-content')),desktopNavigation:rect(document.querySelector('.main-nav')),
  navigation:rect(document.querySelector('.mobile-nav')),tools:rect(document.querySelector('#tools')),
  padding:{top:parseFloat(style.paddingTop),right:parseFloat(style.paddingRight),bottom:parseFloat(style.paddingBottom),left:parseFloat(style.paddingLeft)},
  menu:{...rect(menu),visible:menu.getBoundingClientRect().width>0,scrollHeight:menu.scrollHeight,clientHeight:menu.clientHeight,scrollWidth:menu.scrollWidth,clientWidth:menu.clientWidth},
  menuButtons:[...menu.querySelectorAll('button')].map(b=>({label:b.textContent,...rect(b),fontSize:getComputedStyle(b).fontSize}))};
 window.__geometry=data;
 window.webkit?.messageHandlers?.geometry?.postMessage(data);
};
document.querySelector('#tools').addEventListener('click',()=>{const menu=document.querySelector('#tools-menu');menu.style.display=menu.style.display==='none'?'flex':'none';measureGeometry();});
document.querySelector('#tools-menu').addEventListener('scroll',()=>measureGeometry());
addEventListener('resize',()=>requestAnimationFrame(measureGeometry));
addEventListener('load',()=>requestAnimationFrame(measureGeometry));
'''


def source(ref, path):
    return subprocess.check_output(['git', '-C', str(ROOT), 'show', f'{ref}:{path}'])


def generate(output):
    output.mkdir(parents=True, exist_ok=True)
    current = subprocess.check_output(['git', '-C', str(ROOT), 'rev-parse', 'HEAD'], text=True).strip()
    manifest = {'schema_version': 1, 'kind': 'source-matched-shell-geometry', 'current_source': current,
                'baseline_source': BASELINE, 'scope': 'Native WebKit layout fixture using exact shell CSS/viewport. No actual OCR application execution.', 'sources': {}, 'files': {}}
    for variant, ref in [('baseline', BASELINE), ('current', current)]:
        files = {path: source(ref, path) for path in PATHS}
        if variant == 'current' and any((ROOT / path).read_bytes() != data for path, data in files.items()):
            raise ValueError('Commit shell source changes before generating the fixture')
        manifest['sources'][variant] = {path: hashlib.sha256(data).hexdigest() for path, data in files.items()}
        meta = re.search(r'<meta name="viewport" content="([^"]+)"', files['src/app.html'].decode()).group(1)
        styles = re.findall(r'<style>(.*?)</style>', files['src/routes/+page.svelte'].decode(), flags=re.S)
        if len(styles) != 1:
            raise ValueError('Expected exactly one shell style block')
        buttons = []
        for label in ['Inbox', 'Review', 'Library', 'Chat', 'Tools', 'Settings']:
            icon = '<span aria-hidden="true">⋯</span>' if label == 'Tools' else '<svg width="20" height="20" aria-hidden="true"><rect x="2" y="2" width="16" height="16" stroke="currentColor" fill="none"/></svg>'
            ident = ' id="tools"' if label == 'Tools' else ''
            buttons.append(f'<button class="mobile-tab"{ident}>{icon}<span>{label}</span></button>')
        content = (f'<!doctype html><html><head><meta charset="utf-8"><meta name="viewport" content="{html.escape(meta)}">'
                   f'<style>{files["src/app.css"].decode()}\n{styles[0]}</style></head><body data-variant="{variant}">'
                   + BODY.replace('__BUTTONS__', ''.join(buttons)) + f'<script>{SCRIPT}</script></body></html>')
        path = output / f'{variant}.html'
        path.write_text(content)
        manifest['files'][path.name] = {'bytes': path.stat().st_size, 'sha256': hashlib.sha256(path.read_bytes()).hexdigest()}
    (output / 'source.json').write_text(json.dumps(manifest, indent=2) + '\n')
    return manifest


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', required=True, type=Path)
    args = parser.parse_args()
    print(json.dumps(generate(args.output)))
