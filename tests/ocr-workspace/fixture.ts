import { addDocument, applyRecognition, approvePage, createProject, type OcrProject, type RecognitionResult } from '../../src/lib/ocr/model';
import type { ExportRequest, ExportOptions, ExportProgress, ProjectEnvelope, WorkspaceClient, ModelDownloadProgress, ModelStorageReport, ImportOptions, ImportProgress } from '../../src/lib/ocr/client';
import { beginBatch, updateBatchPage } from '../../src/lib/ocr/batch';
import { photoQuadError } from '../../src/lib/ocr/photo';

const copy = <T>(value:T):T => JSON.parse(JSON.stringify(value));
const stamp = '2026-11-08T12:00:00.000Z';
const sourceUri = '/fixtures/northstar-statement.pdf';
const pagePath = (page:number) => `/fixtures/northstar-page-${page}.png`;
const pageNumber = (path:string) => Number(path.match(/page-(\d+)/)?.[1] ?? 1);

/** Synthetic fixture: exact source text/geometry, no model or network dependency. */
function recognition(path:string):RecognitionResult {
    const page=pageNumber(path);
    const blocks=[
        {text:'NORTHSTAR SUPPLY CO.',x:80,y:90,w:540,h:40,confidence:0.99},
        {text:`Service statement · page ${page}`,x:80,y:165,w:750,h:52,confidence:0.97},
        {text:'Invoice NS-2026-1042',x:80,y:270,w:640,h:40,confidence:0.96},
        {text:'Issued 08 November 2026',x:80,y:325,w:640,h:38,confidence:0.95},
        {text:'Quarterly equipment inspection and preventive maintenance.',x:80,y:525,w:840,h:75,confidence:0.97},
        {text:'Total due EUR 1,250.00',x:80,y:955,w:650,h:50,confidence:0.92},
    ];
    return {
        image_path:path,width:1000,height:1400,elapsed_ms:80,engine:'surya',
        source_type:'scanned_doc',accepted:true,coordinate_space:'image_pixels',
        regions:blocks.map(block=>({...block,confidence_basis:'recognition',char_conf:[],char_conf_unit:'unknown',char_conf_aligned:false})),
    };
}

function documentSvg(page:number):string {
    return `<svg xmlns="http://www.w3.org/2000/svg" width="1000" height="1400" viewBox="0 0 1000 1400">
      <rect width="1000" height="1400" fill="#fffefa"/>
      <rect x="78" y="58" width="66" height="7" fill="#2e6a80"/>
      <g fill="#263e50" font-family="Arial,sans-serif">
        <text x="80" y="122" font-size="31" font-weight="700" letter-spacing="2">NORTHSTAR SUPPLY CO.</text>
        <text x="80" y="204" font-size="37" font-weight="700">Service statement · page ${page}</text>
        <text x="80" y="299" font-size="28">Invoice NS-2026-1042</text>
        <text x="80" y="354" font-size="25">Issued 08 November 2026</text>
        <text x="80" y="430" font-size="20" fill="#657485">BILL TO</text>
        <text x="80" y="470" font-size="26">Orchard Research Group</text>
        <text x="80" y="554" font-size="25">Quarterly equipment inspection and</text>
        <text x="80" y="590" font-size="25">preventive maintenance.</text>
        <rect x="80" y="665" width="840" height="54" fill="#edf2f3"/>
        <text x="101" y="700" font-size="21" font-weight="700">DESCRIPTION</text><text x="754" y="700" font-size="21" font-weight="700">AMOUNT</text>
        <text x="101" y="763" font-size="23">Inspection and testing</text><text x="754" y="763" font-size="23">950.00</text>
        <text x="101" y="826" font-size="23">Maintenance supplies</text><text x="754" y="826" font-size="23">300.00</text>
        <path d="M80 852 H920" stroke="#c9d1d6" stroke-width="2"/>
        <text x="80" y="994" font-size="31" font-weight="700">Total due EUR 1,250.00</text>
        <text x="80" y="1095" font-size="22" fill="#657485">Payment due within 30 days of issue.</text>
        <text x="80" y="1268" font-size="18" fill="#7e8992">Northstar Supply Co. · Operations and service</text>
        <text x="887" y="1268" font-size="18" fill="#7e8992">${page}</text>
      </g>
    </svg>`;
}

export interface SmokeControl {
    calls: Array<{method:string; data:unknown}>;
    saves: ProjectEnvelope[];
    exports: ExportRequest[];
    recognitions: string[];
    activeNative: number;
    maxNative: number;
    holdRecognition: boolean;
    holdExtraction: boolean;
    pendingExtractions: number;
    holdSaves: boolean;
    holdFinalSave: boolean;
    pendingSaves: number;
    failSaves: boolean;
    failPages: number[];
    modelInstalls: string[];
    installedModels: string[];
    pendingModelInstalls: number;
    failModelInstall: boolean;
    modelStorageReport: ModelStorageReport;
    holdModelStorage: boolean;
    pendingModelStorage: number;
    failModelStorage: boolean;
    releaseModelStorage(): boolean;
    releaseModelInstall(): boolean;
    emitModelProgress(progress: ModelDownloadProgress): boolean;
    pendingImports: number;
    cancelledImports: number;
    releaseImport(): boolean;
    emitImportProgress(progress: ImportProgress): boolean;
    pendingExports: number;
    cancelledExports: number;
    completedExports: number;
    failExport: boolean;
    releaseExport(): boolean;
    emitExportProgress(progress: ExportProgress): boolean;
    released: string[];
    releaseNext():boolean;
    releaseExtraction():boolean;
    releaseSave():boolean;
    writes: Array<{outputPath:string;text:string;sourceUris:string[]}>;
    lastSaved: OcrProject | null;
}

declare global {
    interface Window { ocrSmoke:SmokeControl; __ocrInjected?:boolean; }
}

export function createFixtureClient(scenario:string): {client:WorkspaceClient;initialProject:OcrProject|null} {
    const rtlExport = scenario.startsWith('export-rtl');
    const archivalExport = scenario === 'export-archive' || rtlExport || scenario === 'export-unicode' || scenario === 'export-invalid-text';
    const wordExport = scenario !== 'export-rtl-limited';
    let project=scenario==='mobile-models'?createProject('New mobile scan'):addDocument(createProject('Northstar · quarterly statement'),{
        source_uri:sourceUri,name:'Northstar service statement.pdf',
        pages:[1,2,3].map(page=>({image_path:pagePath(page),width:1000,height:1400})),
    });
    project.id=`ocr-fixture-${scenario}`;project.createdAt=stamp;project.updatedAt=stamp;
    if(project.pages.length && !scenario.startsWith('queue')) {
        const result=recognition(project.pages[0].imagePath);
        result.regions[2].text='Inv0ice NS-2026-1042';result.regions[2].confidence=0.42;
        project=applyRecognition(project,project.pages[0].id,result);
        const last = recognition(project.pages[2].imagePath);
        if (rtlExport) last.regions[2].text = '  مَرْحَبًا ABC 123\t\n\nשָׁלוֹם é 𞤀𞤢  ';
        if (scenario === 'export-unicode') {
            last.regions[2].text = 'Reviewed café 中文 😀 é';
            last.regions[4].text = 'x'.repeat(50_000);
        }
        if (scenario === 'export-invalid-text') last.regions[2].text = 'Reviewed\u0000 العربية\fשלום';
        project=applyRecognition(project,project.pages[2].id,last);
        project=approvePage(project,project.pages[2].id);
    }
    if(scenario==='batch-recovery') {
        // The stored intent selects only the first two pages; the third stays
        // pending and current toolbar settings intentionally differ.
        project.pages[2]={...project.pages[2],status:'pending',regions:[],approved:false};
        project=beginBatch(project,project.pages.slice(0,2).map(page=>page.id),{
            profileId:'balanced',engine:'tesseract',language:'deu',config:{source_type:'photo'},
        });
        project=updateBatchPage(project,project.pages[0].id,'completed');
        project=updateBatchPage(project,project.pages[1].id,'running');
        project.pages[1]={...project.pages[1],status:'running'};
        project.recognitionSettings={engine:'got',language:'eng',config:{source_type:'screenshot'}};
    }
    const savedById=new Map<string,ProjectEnvelope>();
    const initialEnvelope:ProjectEnvelope={project:copy(project),revision:7,saved_at:Date.parse(stamp),path:'/fixtures/project.json'};
    savedById.set(project.id,initialEnvelope);
    if(scenario==='project-recovery') {
        // Simulate a visible saved project from an unsupported future format.
        // The adapter intentionally returns invalid input at this trust boundary.
        const invalid=copy(project);
        invalid.id='ocr-fixture-invalid';invalid.name='Invalid archived project';
        (invalid as {version:number}).version=99;
        savedById.set(invalid.id,{...initialEnvelope,project:invalid});
    }
    const pending:Array<{path:string;release:()=>void}>=[];
    const pendingExtractions:Array<()=>void>=[];
    const pendingSaves:Array<()=>void>=[];
    const pendingModelInstalls:Array<{release:()=>void;onProgress?:(progress:ModelDownloadProgress)=>void}>=[];
    const pendingModelStorage:Array<()=>void>=[];
    const pendingImports:Array<{release:()=>void;options?:ImportOptions}>=[];
    const pendingExports:Array<{release:()=>void;options?:ExportOptions;phase:ExportProgress['phase'];cancelled:boolean}>=[];
    const control:SmokeControl={
        calls:[],saves:[],exports:[],recognitions:[],activeNative:0,maxNative:0,
        holdRecognition:scenario==='queue-control',holdExtraction:scenario==='extraction-delay',pendingExtractions:0,
        holdSaves:false,holdFinalSave:false,pendingSaves:0,failSaves:false,failPages:scenario==='queue-failure'?[2]:[],released:[],writes:[],lastSaved:null,
        modelInstalls:[],installedModels:[],pendingModelInstalls:0,failModelInstall:false,
        modelStorageReport: scenario==='mobile-storage' ? {
            bytes:16*1024**2,reclaimable_bytes:4*1024**2,warnings:['Unrecognized fixture metadata was kept for inspection.'],
            models:[
                {name:'dbnet-det',version:'11111111-1111-4111-8111-111111111111',bytes:8*1024**2,reclaimable_bytes:0,in_use:true},
                {name:'tesseract-eng',version:'22222222-2222-4222-8222-222222222222',bytes:8*1024**2,reclaimable_bytes:4*1024**2,in_use:false},
            ],
        } : {bytes:0,reclaimable_bytes:0,models:[],warnings:[]},
        holdModelStorage:false,pendingModelStorage:0,failModelStorage:false,
        releaseModelStorage(){const release=pendingModelStorage.shift();if(!release)return false;control.pendingModelStorage=pendingModelStorage.length;release();return true;},
        pendingImports:0,cancelledImports:0,
        releaseImport(){const item=pendingImports.shift();if(!item)return false;control.pendingImports=pendingImports.length;item.release();return true;},
        emitImportProgress(progress){const item=pendingImports[0];if(!item)return false;item.options?.onProgress?.(copy(progress));return true;},
        pendingExports:0,cancelledExports:0,completedExports:0,failExport:false,
        releaseExport(){const item=pendingExports.shift();if(!item)return false;control.pendingExports=pendingExports.length;item.release();return true;},
        emitExportProgress(progress){const item=pendingExports[0];if(!item)return false;item.phase=progress.phase;item.options?.onProgress?.(copy(progress));return true;},
        releaseModelInstall(){const item=pendingModelInstalls.shift();if(!item)return false;control.pendingModelInstalls=pendingModelInstalls.length;item.release();return true;},
        emitModelProgress(progress){const item=pendingModelInstalls[0];if(!item)return false;item.onProgress?.(copy(progress));return true;},
        releaseNext(){const item=pending.shift();if(!item)return false;control.released.push(item.path);item.release();return true;},
        releaseExtraction(){const release=pendingExtractions.shift();if(!release)return false;control.pendingExtractions=pendingExtractions.length;release();return true;},
        releaseSave(){const release=pendingSaves.shift();if(!release)return false;control.pendingSaves=pendingSaves.length;release();return true;},
    };
    window.ocrSmoke=control;
    const note=(method:string,data:unknown={})=>control.calls.push({method,data:copy(data)});
    const waitForExtraction=async()=>{
        if(control.holdExtraction)await new Promise<void>(release=>{
            pendingExtractions.push(release);control.pendingExtractions=pendingExtractions.length;
        });
    };
    const waitForStorage=async()=>{
        if(control.holdModelStorage)await new Promise<void>(release=>{
            pendingModelStorage.push(release);control.pendingModelStorage=pendingModelStorage.length;
        });
        if(control.failModelStorage)throw new Error('Fixture storage is busy in another application. Retry cleanup.');
    };
    const client:WorkspaceClient={
        async capabilities(){note('capabilities');return {ocr:true,crispembed:true,pdf_import:true,formats:['pdf',...(archivalExport?['pdfa']:[]),'text','json','html','markdown','hocr','alto',...(wordExport?['docx']:[])],archival_pdf_export:archivalExport,pdf_bidi_export:!rtlExport,docx_export:wordExport,pdfa_export_reason:archivalExport?undefined:'PDF/A-2b requires the archival-capable native renderer in this build.',engines:['dbnet_trocr','surya','tesseract','got','glm','qwen2vl','qwen3vl','internvl2'],layout:true,kie:true,table:true,math:true,cpu_budget:true,cpu_budget_pixels:262144,photo_tools:true,camera_import:true,...(['mobile-models','mobile-storage'].includes(scenario)?{mobile_model_store:true,default_engine:'tesseract',default_language:'eng',pdf_import:false}:{})};},
        async modelStorage(){note('modelStorage');return copy(control.modelStorageReport);},
        async cleanModelStorage(){
            note('cleanModelStorage');await waitForStorage();
            const removed_bytes=control.modelStorageReport.reclaimable_bytes;
            control.modelStorageReport={...control.modelStorageReport,bytes:control.modelStorageReport.bytes-removed_bytes,reclaimable_bytes:0,
                models:control.modelStorageReport.models.map(model=>({...model,bytes:model.bytes-model.reclaimable_bytes,reclaimable_bytes:0}))};
            return {removed_bytes,removed_versions:removed_bytes?2:0,report:copy(control.modelStorageReport)};
        },
        async removeMobileModel(name,expectedVersion){
            note('removeMobileModel',{name,expectedVersion});await waitForStorage();
            const current=control.modelStorageReport.models.find(model=>model.name===name);
            if(!current || current.version!==expectedVersion)throw new Error('This model changed after the storage view was opened. Refresh storage before removing it.');
            if(current.in_use)throw new Error('This model is in use by OCR. Restart before removing it.');
            control.modelStorageReport={...control.modelStorageReport,bytes:control.modelStorageReport.bytes-current.bytes,
                reclaimable_bytes:control.modelStorageReport.reclaimable_bytes-current.reclaimable_bytes,
                models:control.modelStorageReport.models.filter(model=>model.name!==name)};
            return {removed_bytes:current.bytes,removed_versions:1,report:copy(control.modelStorageReport)};
        },
        async list(){note('list');return [...savedById.values()].map(saved=>({id:saved.project.id,name:saved.project.name,updated_at:saved.saved_at,page_count:saved.project.pages.length,revision:saved.revision}));},
        async load(projectId){note('load',{projectId});return copy(savedById.get(projectId??project.id)??null);},
        async save(snapshot,expectedRevision){
            note('save',{id:snapshot.id,expectedRevision});
            if(control.holdSaves || (control.holdFinalSave && snapshot.batch?.phase==='completed'))await new Promise<void>(release=>{
                pendingSaves.push(release);control.pendingSaves=pendingSaves.length;
            });
            if(control.failSaves)throw new Error('Disk full: the project checkpoint could not be saved.');
            const previous=savedById.get(snapshot.id)?.revision??0;
            if(previous!==expectedRevision)throw new Error(`Project revision conflict: expected ${previous}, received ${expectedRevision}`);
            const saved={project:copy(snapshot),revision:previous+1,saved_at:Date.now(),path:'/fixtures/project.json'};
            savedById.set(snapshot.id,saved);control.saves.push(copy(saved));control.lastSaved=copy(snapshot);return copy(saved);
        },
        async importDocument(locationUri,options){
            note('importDocument',{locationUri});
            if(scenario==='import-control'){
                const onAbort=()=>{control.cancelledImports++;};
                options?.signal?.addEventListener('abort',onAbort,{once:true});
                control.activeNative++;control.maxNative=Math.max(control.maxNative,control.activeNative);
                try{
                    await new Promise<void>(release=>{pendingImports.push({release,options});control.pendingImports=pendingImports.length;});
                    if(options?.signal?.aborted)throw new DOMException('Document import cancelled.','AbortError');
                }finally{control.activeNative--;options?.signal?.removeEventListener('abort',onAbort);}
                return {source_uri:locationUri,archive_uri:`/fixtures/archive/${locationUri.split('/').pop()}`,name:locationUri.split('/').pop()!,
                    pages:[4,5].map(page=>({image_path:pagePath(page),width:1000,height:1400}))};
            }
            return {source_uri:locationUri,name:locationUri.split('/').pop()!,pages:[{image_path:pagePath(4),width:1000,height:1400}]};
        },
        async importImage(name,bytes){note('importImage',{name,byteLength:bytes.length});return {source_uri:'/fixtures/captured-photo.png',name,pages:[{image_path:'/fixtures/captured-photo.png',width:1000,height:1400}]};},
        async assessPhoto(path){
            note('assessPhoto',{pagePath:path});
            return {width:1000,height:1400,sample_width:786,sample_height:1100,mean_luminance:173,contrast:58,sharpness:102,
                ink_fraction:0.13,correction_degrees:-2.7,skew_confidence:0.91,warnings:['Uneven lighting: inspect the page after flattening shadows.']};
        },
        async installMobileModels(language,onProgress){
            note('installMobileModels',{language});control.modelInstalls.push(language);
            await new Promise<void>(release=>{pendingModelInstalls.push({release,onProgress});control.pendingModelInstalls=pendingModelInstalls.length;});
            if(control.failModelInstall)throw new Error('Fixture model download interrupted. Retry the installation.');
            const names=['dbnet-det',`tesseract-${language}`];control.installedModels=[...names];return names;
        },
        async recognize(options){
            note('recognize',options);control.recognitions.push(options.pagePath);control.activeNative++;control.maxNative=Math.max(control.maxNative,control.activeNative);
            try {
                if(scenario==='mobile-models' && !control.installedModels.includes(`tesseract-${options.language??'eng'}`))throw new Error('The requested OCR models are not installed.');
                if(control.holdRecognition)await new Promise<void>(release=>pending.push({path:options.pagePath,release}));
                if(control.failPages.includes(pageNumber(options.pagePath)))throw new Error('Fixture recognition failure on page 2');
                return recognition(options.pagePath);
            } finally {control.activeNative--;}
        },
        async transform(options){
            note('transform',options);
            if(options.operation==='split')return {pages:[{image_path:options.pagePath+'-left',width:500,height:1400},{image_path:options.pagePath+'-right',width:500,height:1400}]};
            if(options.operation==='rotate')return {pages:[{image_path:options.pagePath+'-rotated',width:1400,height:1000}]};
            if(options.operation==='perspective') {
                const error=photoQuadError(options.quad??[],1000,1400);
                if(error)throw new Error(error);
                return {pages:[{image_path:options.pagePath+'-perspective',width:900,height:1300,warnings:['Perspective corrected from four user-selected corners.']}]};
            }
            if(options.operation==='deskew') {
                if(!Number.isFinite(options.angle)||Math.abs(options.angle??0)>20)throw new Error('Choose a finite angle between -20 and 20 degrees.');
                return {pages:[{image_path:options.pagePath+'-deskewed',width:1065,height:1447,warnings:['Tilt corrected using the chosen angle.']}]};
            }
            return {pages:[{image_path:options.pagePath+'-crop',width:Math.round(options.crop?.w??1000),height:Math.round(options.crop?.h??1400)}]};
        },
        async prepare(path,config,pageWidthPoints,pageHeightPoints){
            note('prepare',{path,config,pageWidthPoints,pageHeightPoints});
            if(scenario==='prepare-unchanged')return {
                image_path:path,width:1000,height:1400,
                page_width_points:pageWidthPoints,page_height_points:pageHeightPoints,
                warnings:['Cleanup unavailable; original pixels retained.'],
            };
            return {image_path:path+'-prepared',width:1000,height:1400};
        },
        async exportDocument(request,options){
            note('exportDocument',request);control.exports.push(copy(request));
            if(scenario==='export-control'){
                const item:{release:()=>void;options?:ExportOptions;phase:ExportProgress['phase'];cancelled:boolean}={release:()=>{},options,phase:'rendering',cancelled:false};
                const onAbort=()=>{if(item.phase!=='committing'&&item.phase!=='destination'){item.cancelled=true;control.cancelledExports++;}};
                options?.signal?.addEventListener('abort',onAbort,{once:true});
                control.activeNative++;control.maxNative=Math.max(control.maxNative,control.activeNative);
                try{
                    if(options?.signal?.aborted)onAbort();
                    // A stop request does not finish the native worker. The test
                    // controls that boundary separately from the AbortSignal.
                    await new Promise<void>(release=>{item.release=release;pendingExports.push(item);control.pendingExports=pendingExports.length;});
                    if(item.cancelled)throw new DOMException('Document export cancelled.','AbortError');
                    if(control.failExport)throw new Error('Fixture export could not finish writing.');
                }finally{control.activeNative--;options?.signal?.removeEventListener('abort',onAbort);}
            }
            control.completedExports++;
            return {saved_path:request.outputPath,bytes:1024,pages:request.pages.length,format:request.format};
        },
        async extractFields(text,labels,threshold){note('extractFields',{text,labels,threshold});await waitForExtraction();return [{label:'vendor',value:'Northstar Supply Co.',score:0.97},{label:'total',value:'EUR 1,250.00',score:0.95}];},
        async layoutFields(pagePath,labels,threshold){note('layoutFields',{pagePath,labels,threshold});await waitForExtraction();return [{label:'invoice number',value:'NS-2026-1042',score:0.94}];},
        async table(path){note('table',{path});await waitForExtraction();return {html:'<table><tr><th>Item</th><th>Value</th></tr><tr><td><img src="bad" onerror="window.__ocrInjected=true">Secure cell</td><td><a href="javascript:window.__ocrInjected=true">EUR 1,250.00</a></td></tr></table><script>window.__ocrInjected=true</script>',rows:2,cols:2};},
        async math(path){note('math',{path});await waitForExtraction();return String.raw`E = mc^2`;},
        async reindex(sourceUri,text){note('reindex',{sourceUri,text});return {documents:1};},
        async pickDocuments(){note('pickDocuments');return scenario==='import-control'?['/fixtures/first.pdf','/fixtures/second.pdf','/fixtures/third.pdf']:['/fixtures/added-scan.png'];},
        async pickExport(name,extension){note('pickExport',{name,extension});return `/fixtures/${name}.${extension}`;},
        async writeText(outputPath,text,sourceUris){const request={outputPath,text,sourceUris};note('writeText',request);control.writes.push(copy(request));return {saved_path:outputPath,bytes:new TextEncoder().encode(text).length};},
        imageUrl(path){return `data:image/svg+xml;charset=utf-8,${encodeURIComponent(documentSvg(pageNumber(path)))}`;},
    };
    return {client,initialProject:null};
}
