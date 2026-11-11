import ctypes as C
import json
import subprocess
from pathlib import Path
from xml.etree import ElementTree as ET

class Word(C.Structure):
    _fields_=[('text',C.c_char_p),('x',C.c_int),('y',C.c_int),('w',C.c_int),('h',C.c_int),('confidence',C.c_float)]
class Line(C.Structure):
    _fields_=[('words',C.POINTER(Word)),('n_words',C.c_int),('x',C.c_int),('y',C.c_int),('w',C.c_int),('h',C.c_int)]
class Page(C.Structure):
    _fields_=[('lines',C.POINTER(Line)),('n_lines',C.c_int),('page_width',C.c_int),('page_height',C.c_int),('image_path',C.c_char_p)]
lib=C.CDLL(str(Path('libocr_render_test.so').resolve()))
for name,args,result in [
    ('ocr_render_create',[C.c_int],C.c_void_p),
    ('ocr_render_begin',[C.c_void_p],None),
    ('ocr_render_add_page',[C.c_void_p,C.POINTER(Page)],None),
    ('ocr_render_end',[C.c_void_p],None),
    ('ocr_render_error',[C.c_void_p],C.c_char_p),
    ('ocr_render_output',[C.c_void_p],C.c_void_p),
    ('ocr_render_output_size',[C.c_void_p],C.c_int),
    ('ocr_render_free',[C.c_void_p],None),
]:
    fn=getattr(lib,name);fn.argtypes=args;fn.restype=result
text='First résumé\nSecond 東京'
word=Word(text.encode(),20,20,280,44,0.85)
line=Line(C.pointer(word),1,20,20,280,44)
page=Page(C.pointer(line),1,320,200,None)
results={}
for fmt,name in [(0,'txt'),(1,'hocr'),(2,'alto'),(3,'pdf')]:
    renderer=lib.ocr_render_create(fmt)
    try:
        lib.ocr_render_begin(renderer)
        lib.ocr_render_add_page(renderer,C.byref(page))
        lib.ocr_render_end(renderer)
        assert not lib.ocr_render_error(renderer),lib.ocr_render_error(renderer)
        data=C.string_at(lib.ocr_render_output(renderer),lib.ocr_render_output_size(renderer))
    finally:
        lib.ocr_render_free(renderer)
    target=Path('multiline.'+name);target.write_bytes(data)
    if name=='txt': assert data.decode().rstrip('\n')==text
    elif name=='hocr':
        root=ET.fromstring(data)
        words=[el.text for el in root.iter() if el.attrib.get('class')=='ocrx_word']
        assert words==[text],words
    elif name=='alto':
        root=ET.fromstring(data)
        words=[el.attrib['CONTENT'] for el in root.iter() if el.tag.endswith('}String')]
        assert words==[text],words
    else:
        extracted=subprocess.check_output(['pdftotext',str(target),'-'],text=True)
        assert text in extracted,repr(extracted)
        from pypdf import PdfReader
        from pypdf.generic import ContentStream
        reader=PdfReader(target)
        assert text in reader.pages[0].extract_text()
        baselines=[float(args[5]) for args,op in ContentStream(reader.pages[0]['/Contents'],reader).operations if op==b'Tm']
        assert len(baselines)==2 and baselines[0]>baselines[1],baselines
        results['pdf_baselines']=baselines
    results[name]='passed'
receipt={'source_commit':'e14136d3a99572367e68619045943fe77a299caa','source':'src/ocr_render.cpp','raster_decoder_tested':False,'app_rust_compiled':False,'multiline_region':results}
if Path('source-pins.json').exists(): receipt['sha256'] = json.loads(Path('source-pins.json').read_text())
Path('receipt.json').write_text(json.dumps(receipt,ensure_ascii=False,indent=2)+'\n')
print(json.dumps(receipt))
